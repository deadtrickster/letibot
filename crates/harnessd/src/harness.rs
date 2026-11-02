//! **The loop.** Submit → stream → parse a tool call → execute it → append the
//! result → resubmit.
//!
//! ```text
//!   user item ──append──> ledger ──submit──> stream ──parse──> items
//!        ▲                                                       │
//!        │                                              tool calls?
//!        │                                                 │        └── no ──> reply
//!   ToolResult item <──transcript_item── ToolRuntime::invoke ── yes
//! ```
//!
//! `ToolRuntime::transcript_item` and `ToolLogSink` were the two ends of this wire
//! and both were tested. This file is what runs between them.
//!
//! # Three things this file had to decide that no crate had decided
//!
//! **1. Item ids are minted by two different rules, and the head needs both.**
//! `Session::append_items` mints `"{transcript}.{index}"`; the turn engine mints
//! `"{turn_id}.{n}"`. §4.5's `TranscriptAppended` carries no content, so the daemon
//! has to call `Hub::record_item(item_id, item)` out of band — and it must use the
//! *same* id the event carried, or the head shows an empty row forever. Rather than
//! reimplementing both rules and going stale, [`CapturingSink`] reads the id off
//! the event on its way past. That is not a workaround for T13.1; T13.1 is the
//! event having no content channel, and widening it unilaterally is not this
//! strand's call.
//!
//! **2. A failed turn leaves nothing to answer.** When §5.7 refuses a batch, the
//! engine commits *nothing* — so there are no tool-call rows to attach pi's
//! "re-issue with complete arguments" notice to. The notice therefore goes in as a
//! plain user item, which is the only append-only place it fits, and the loop
//! retries under the engine's own salvage budget rather than a second counter.
//!
//! **3. A tool batch is executed in call order and appended in call order.** Not a
//! style choice: GLM's shipped template *sorts* tool results by tool-call order, so
//! appending them in any other order would rewrite bytes already in the KV cache.
//! Keeping call order makes the template's sort a no-op and both properties hold at
//! once.

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
    AuthorisationTrail, BreakerState, DenialNotice, DenialSink, Speaker, Utterance,
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

/// How many of the operator's own utterances the trail carries.
///
/// Bounded for the reason every body in this tree is bounded, and the bound is
/// small because §2's evidence is *recent*: the sentence that authorised an action
/// is nearly always the last one or two. What the cap must never do is make an
/// **uncollected** trail look like a short one, and it cannot — the denominator
/// travels separately and counts every message scanned.
const TRAIL_UTTERANCES: usize = 12;

/// How far back the walk goes. A session with 400 turns has an authorisation
/// somewhere in the last handful, and scanning all of it to find twelve is work
/// done inside the tool path.
///
/// **It is also how many utterances the trail HOLDS** — see [`TrailMirror::say`].
/// Keeping more than this was pure waste and an unbounded per-session term: the
/// only reader takes the last `TRAIL_SCAN`, so everything older than the sixtieth
/// was held for the life of the session and never read once.
pub(crate) const TRAIL_SCAN: usize = 60;

/// The longest an utterance travels verbatim, matching
/// [`letibot_tools::authorise::Utterance::operator`]'s own rule. A truncated
/// authorisation that reads as complete is worse than a missing one, so a clipped
/// one says it was clipped.
///
/// **And it is how much of an utterance is STORED** — see [`TrailMirror::say`]. What
/// travels was always clipped to this; holding the rest cost the daemon memory no reader
/// ever looked at.
pub(crate) const UTTERANCE_CHARS: usize = 600;

/// Everything the engine borrows for the life of the daemon.
///
/// Held apart from [`Harness`] because `TurnEngine<'a>` borrows the vocabulary and
/// the renderer, and a struct owning both the borrow and the borrowed would be
/// self-referential. The caller keeps this on the stack and hands out a reference,
/// which is a two-line inconvenience against a `Box::leak` that lies about
/// lifetimes.
/// Every field is an `Arc`, so a clone is a handful of refcount bumps — which
/// is what lets a second head (the HTTP one) hold the same vocabulary and dialect
/// without loading either again.
#[derive(Clone)]
pub struct Parts {
    pub vocab: std::sync::Arc<Vocab>,
    pub wiring: std::sync::Arc<Wiring>,
    /// Per-project mode store, loaded once at daemon start. The mode is a session
    /// property: a session's point is its project's row (longest ancestor wins), not
    /// the daemon's `--mode` flag, so moving a project does not mean restarting the
    /// daemon. Wrapped in a lock because the `/mode` command writes it at run time
    /// while session opens read it. See `D13`.
    pub mode_store: std::sync::Arc<std::sync::RwLock<crate::modes::ModeStore>>,
    /// The subagent (task) journal, shared with every subagent runner so spawns and
    /// finishes land in one place and reach the dashboard's state file.
    pub tasks: std::sync::Arc<crate::tasks::TaskJournal>,
    /// The language servers this daemon can reach, shared with the `lsp` tool and
    /// the dashboard's `lsp` panel.
    pub lsp: std::sync::Arc<letibot_tools::builtins::lsp::LspConfig>,
    /// The loaded skills, shared with the `skill` tool and the dashboard's `skills`
    /// panel.
    pub skills: std::sync::Arc<letibot_tools::builtins::skill::SkillRegistry>,
    /// **Every live todo board this daemon holds, by session id** — the map a parent's
    /// `todo_write` with a `target` resolves its child through (`crate::child_todos`).
    ///
    /// It rides [`Parts`] for the same reason [`Parts::tree_slots`] does: a live `Arc` that
    /// belongs to the DAEMON and not to any one session's config, handed to every harness so
    /// each can register its own board at open — root, child, resumed or fresh — and read any
    /// other's through the resolver built beside it. The store path rides along because the
    /// resolver's flush runs on another thread and reaches the store by path — a handle onto
    /// the process's one connection, not a second one.
    pub child_boards: std::sync::Arc<crate::child_todos::ChildBoards>,
    /// **The watcher set of the subagent TREE this session belongs to, when it is a
    /// child** (R58).
    ///
    /// `None` for a root: a root's watcher set is its own, built at open. `Some` for a
    /// child, carrying its parent's, so the child's set can join the tree's — sharing the
    /// watching/settled bookkeeping, **the level above** (whose queue what this session can
    /// no longer drain goes to, and whose id the ring for it names) and the **ROOT**, which
    /// is the one session in a tree that has a head and is therefore the one a permission
    /// card can be drawn by (`tree_root`).
    ///
    /// **The RING is not among them any more** (2026-10-06). It used to carry the root's id
    /// so a grandchild's settlement would ring a session `Sessions::open` holds, because a
    /// ring for a child was `Ignored` — the condition fired and was discarded. The ring now
    /// names the session that OWNS the settlement (the parent, at every depth) and the
    /// daemon serves a session it does not hold by handing the wake to the thread that does
    /// (`Sessions::wake` → `Hub::wake_its_own_reader`), so a card and an exit are two
    /// questions with two answers. See `jobwatch`'s module header.
    ///
    /// **Not the completions queue** — each session drains its own (2026-10-05). A tree
    /// whose settlements all landed on the root's queue delivered a subagent's background
    /// job to the main session as *"a job you backgrounded"*, and left the subagent that
    /// started it never told; see [`crate::jobwatch::JobWatchers::shares_tree`] for what
    /// the tree still shares and why the ring alone is enough.
    ///
    /// It rides [`Parts`] rather than [`Config`] because a config is *this* session's
    /// facts and a watcher set is a live `Arc`, not a value to clone per session.
    pub tree_watch: Option<std::sync::Arc<crate::jobwatch::JobWatchers>>,
    /// **The subagent handle list a child of this session shares** (R58).
    ///
    /// `None` for a root — its runner gets a fresh list. `Some(parent's)` for a child, so
    /// the whole tree hands out and collects ONE list: `task_result` and `job_kill` on a
    /// grandchild's handle then work from any ancestor, which is the requirement's own
    /// *"every ancestor's handle can be collected from any level"*. Without it the tree's
    /// wake delivers a settlement the root was told about and cannot collect — the
    /// completion names a handle no runner in `open` holds.
    pub(crate) tree_slots: Option<TaskSlots>,
    /// **The head's answers, as the whole tree reaches them** — the ask route's half of
    /// R58.
    ///
    /// The operator, on where a subagent's permission ask goes: *"who asks subagents
    /// permissions? i think they should surface to the parent head all the way to the root
    /// obviously"*. A child has no head, so its gate's card is posted to the session that
    /// does — the tree's ROOT — and the root's own [`crate::answers::Answers`] is what
    /// settles it: posting on the root's hub without the root's sink would leave a card
    /// that nothing can answer.
    ///
    /// `None` for a root, whose own answers are built at open; `Some` for a child, which
    /// inherits its root's and hands the same value to its own children. It rides
    /// [`Parts`] rather than [`Config`] because it is a live `Arc` and not a value to clone
    /// per session — the same reason [`Parts::tree_watch`] does.
    pub tree_head: Option<std::sync::Arc<crate::answers::Answers>>,
    /// **The daemon's review desk** — who is reviewing what, and what they have handed in
    /// ([`letibot_tools::builtins::verdict::ReviewDesk`]).
    ///
    /// It rides [`Parts`] for [`Parts::child_boards`]'s reason exactly: a review has two sides in
    /// two sessions — the queue's session opens it and writes the row, and the gatekeeper child
    /// hands the verdict in — and the child is a session of its own with a harness of its own. One
    /// `Arc` for the daemon is what makes those two sides one act; a desk per harness would be a
    /// different desk on each side of it, and the child's verdict would reach nobody.
    ///
    /// **Not a store.** What the desk holds is a submission in flight; the durable row is the
    /// queue's own (`Store::put_review`), written by the pass that owns that write
    /// (`Harness::serve_reviews` → `write_outcome`). See the module doc for why the states stay
    /// where they were.
    pub reviews: std::sync::Arc<letibot_tools::builtins::verdict::ReviewDesk>,
}

/// How many ledger rows a resume announces between progress ticks.
///
/// See `Harness::republish`: a tick per row overflowed a head's queue and DEMOTED it, so the
/// restore's own progress bar was killed by the events carrying it.
const FILLING_STRIDE: usize = 64;

/// How many transcript rows one store transaction writes (`Harness::persist_reporting`).
///
/// One transaction per row made a 971-row `/reseat` take 6.5 minutes on a store a dozen daemons
/// share; one transaction for everything would hold the box's single write lock for the whole of
/// a long carry. A chunk is the middle: a handful of lock trips for a whole conversation, and the
/// lock released often enough that another daemon's write never waits behind more than one chunk.
///
/// It IS the store's own chunk ([`letibot_tokencore::store::Store::APPEND_CHUNK`], 64, measured
/// on the `sessions.db` volume as the hold that lets a competing writer in): one chunk here is
/// then exactly one `BEGIN IMMEDIATE` transaction in the store, so a committed chunk, a tick of
/// the carry's `Filling` line and the `persisted` count all mean the same rows.
const PERSIST_CHUNK: usize = letibot_tokencore::store::Store::APPEND_CHUNK;

impl Parts {
    pub fn load(cfg: &Config) -> Result<Parts, HarnessError> {
        let wiring = cfg.dialect.wiring(cfg.effort.as_deref());
        // **Which vocabulary, decided by what the turns need.** A GGUF named on the command
        // line is read by llama.cpp — the only vocabulary a local llama-server can take ids
        // from. None named and a provider answering: the byte vocabulary, because there
        // the ids never leave this machine (see `letibot_tokencore::vocab`).
        //
        // **None named and no provider: the byte vocabulary too, and no refusal.** This was a
        // refusal naming both ways out, and it was the daemon's half of the launcher's model
        // gate — a box with no GGUF and no key could not start a session at all. The operator,
        // 2026-10-18, after `leticode` on such a box refused: *"idk why i even have to do
        // --provider. horrible ux - we already have models chooser and keys asker"*. So the
        // daemon starts with no model chosen, the head opens its chooser when the first message
        // is sent (the `vocab` row says `none`), and a TURN on the local route is what is refused
        // — in `rounds`, before anything is sent (`NO_MODEL_CHOSEN`), because a llama-server
        // cannot read byte ids and the guess this used to refuse would still be a guess there.
        let vocab = match &cfg.vocab_gguf {
            Some(path) => {
                if !path.is_file() {
                    return Err(HarnessError::Setup(format!(
                        "no vocabulary GGUF at {}. For a split model, pass the first shard.",
                        path.display()
                    )));
                }
                load_gguf(path)
                    .map_err(|e| HarnessError::Setup(format!("loading the vocabulary: {e}")))?
            }
            None => {
                let spec = wiring.spec();
                Vocab::bytes(
                    spec.control_tokens
                        .as_slice()
                        .iter()
                        .map(|t| t.literal.as_ref())
                        .chain(spec.stop_tokens.iter().map(|t| t.literal.as_ref())),
                    spec.stop_tokens.iter().map(|t| t.literal.as_ref()),
                )
            }
        };
        let skills = std::sync::Arc::new(letibot_tools::builtins::skill::SkillRegistry::load_for(
            &letibot_tools::builtins::skill::SkillDirs::for_this_box(
                Some(&cfg.workspace),
                crate::mergequeue::git::letibot_data_dir().as_deref(),
            ),
        ));
        let lsp = std::sync::Arc::new(letibot_tools::builtins::lsp::LspConfig::default());
        let tasks = std::sync::Arc::new(crate::tasks::TaskJournal::new(
            crate::tasks::default_state_path(),
            lsp.clone(),
            skills.clone(),
        ));
        Ok(Parts {
            vocab: std::sync::Arc::new(vocab),
            wiring: std::sync::Arc::new(wiring),
            mode_store: std::sync::Arc::new(std::sync::RwLock::new(crate::modes::ModeStore::open(
                cfg.store.as_deref(),
            ))),
            tasks,
            lsp,
            skills,
            // The daemon-wide board map, over the same store the sessions write — see the
            // field's doc for why it is here and not on a harness.
            child_boards: crate::child_todos::ChildBoards::shared(cfg.store.clone()),
            // A root session is its own tree; it has no parent's watcher set to join.
            tree_watch: None,
            // Nor a parent's handle list: a root's runner gets a fresh one (R58).
            tree_slots: None,
            // **Nor a parent's head answers**: a root's own are built as its gate is, and a
            // root with no head-wired adjudicator has none to pass down. See `tree_head`.
            tree_head: None,
            // The daemon's review desk, empty: the queue's session opens a review on it when it
            // starts a gatekeeper, and the child hands its verdict back through the same one.
            reviews: std::sync::Arc::new(letibot_tools::builtins::verdict::ReviewDesk::new()),
        })
    }
}

#[derive(Debug)]
pub enum HarnessError {
    Setup(String),
    Turn(TurnFailure),
    Store(String),
    /// The model went round the tool loop `max_tool_rounds` times and was still
    /// producing new results. **The backstop, not a judgement about the work** —
    /// a turn that was going nowhere would have been stopped by
    /// [`HarnessError::NoProgress`] long before this.
    ///
    /// Reported, never silently truncated to whatever the last round happened to
    /// say.
    LoopBound {
        rounds: usize,
    },
    /// The turn stopped because the next round would not fit in the context
    /// window. **Not a failure of the work**: everything already produced is
    /// committed, and `Sessions` compacts before the next turn.
    ///
    /// **`resident` and `window` are in `unit`** — the provider's count on a metered
    /// session, the window as it was given (R72). They used to be the LEDGER's pair, and
    /// the head drew them with no unit: *"stopped after 1 round(s) at the context wall:
    /// 56466 of 41843 tokens resident"* for a 40,000-token deepseek window, while the
    /// daemon's log line for the same wall said *"53978 of 40000 deepseek tokens (56466 of
    /// 41843 ledger tokens)"*. `ledger` keeps the box's own pair for the parenthesis, and
    /// is not printed where it is the same two numbers (a local session).
    ContextWall {
        rounds: usize,
        resident: u64,
        window: u64,
        unit: String,
        ledger: (u64, u64),
    },
    /// **The progress check fired.** `stall_rounds` consecutive rounds produced
    /// nothing this turn had not already seen.
    ///
    /// `evidence` is a sentence naming what the detector saw — the calls, the
    /// outcomes and the denominator — because a stop that cites evidence is one an
    /// operator can contradict and a round count is not. See [`crate::progress`].
    NoProgress {
        evidence: String,
    },
    /// **The endpoint failed, and this is the whole account of it** — the class, how many
    /// times the call was tried and for how long, and the provider's own last words.
    ///
    /// `said` is [`letibot_http::retry::Ladder::ending`]'s sentence, built where the ladder
    /// was run, and it is what the head draws under `FAILED — `. A failure with no count on
    /// it is the report this exists for: *"a child's turn ended with FAILED — http io:
    /// provider unreachable: … (nothing was recorded)"*, which does not say whether the
    /// daemon tried once or six times, and a `401` that reads like a timeout is the same
    /// defect one class along.
    ///
    /// `cause` is kept typed because two questions are asked of a failure AFTER it is
    /// said: `switch_back_notice` cites a 4xx ([`letibot_http::retry::Policy::refusal_of_what_was_sent`])
    /// and a caller may want the status. Nothing else about it is secret — it is in the
    /// sentence too, in the provider's own words.
    Endpoint {
        said: String,
        cause: letibot_turn::HttpError,
    },
}

impl HarnessError {
    /// **The HTTP failure underneath this error**, for the two questions asked after it is
    /// said — never for re-deciding whether to retry, which the ladder already answered.
    pub fn http_cause(&self) -> Option<&letibot_turn::HttpError> {
        match self {
            HarnessError::Turn(TurnFailure::Http(e)) | HarnessError::Endpoint { cause: e, .. } => {
                Some(e)
            }
            _ => None,
        }
    }
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::Setup(s) => write!(f, "{s}"),
            HarnessError::Turn(e) => write!(f, "{e}"),
            // **The whole account of one endpoint failure**: what it was, how many calls it
            // took, and what the provider said last. See [`HarnessError::Endpoint`].
            HarnessError::Endpoint { said, .. } => write!(f, "{said}"),
            HarnessError::Store(s) => write!(f, "store: {s}"),
            // **Never "the model called tools N times without answering".**
            // It *was* answering; it had not finished, and that sentence taught an
            // operator to distrust the model when the harness was at fault — the
            // same defect as `grep` reporting absence when it had opened no files.
            // Reaching here now means every round was still producing new results,
            // which is a fact about the size of the task, not about the model.
            HarnessError::ContextWall {
                rounds,
                resident,
                window,
                unit,
                ledger,
            } => write!(
                f,
                "stopped after {rounds} round(s) at the context wall: {resident} of \
                 {window} {unit} resident{} and the next round would not fit. Everything \
                 this turn produced is committed. A compaction was ATTEMPTED — whether \
                 it succeeded is its own line above, because a summary turn can refuse \
                 (it may not call tools) and saying it worked when it did not is the \
                 failure this whole check exists to avoid. If it compacted, the turn \
                 continues on the summary by itself, a bounded number of times; if the \
                 wall comes back after those, the task does not fit the window and \
                 `/compact` or a fresh session is the way past it.",
                if *ledger == (*resident, *window) {
                    String::new()
                } else {
                    format!(" ({} of {} ledger tokens)", ledger.0, ledger.1)
                }
            ),
            HarnessError::LoopBound { rounds } => write!(
                f,
                "stopped after {rounds} rounds — the round backstop, and it is not a \
                 judgement about the work: the turn was still producing results it had \
                 not seen before when the bound was reached. The progress check did not \
                 fire. Raise --max-tool-rounds if the task is this big."
            ),
            HarnessError::NoProgress { evidence } => write!(f, "{evidence}"),
        }
    }
}

impl std::error::Error for HarnessError {}

impl From<TurnFailure> for HarnessError {
    fn from(e: TurnFailure) -> Self {
        HarnessError::Turn(e)
    }
}

impl From<letibot_turn::EngineError> for HarnessError {
    fn from(e: letibot_turn::EngineError) -> Self {
        HarnessError::Turn(TurnFailure::Engine(e))
    }
}

/// **Which question a compaction door is answering.**
///
/// Two questions share `Config::at_the_wall`'s arithmetic and differ in exactly
/// one gate, which is why they are one body with a parameter
/// (`Harness::compact_at_the_wall`) rather than two functions with copied
/// thresholds — a copy is the drift this tree deletes:
///
/// * [`WallWhy::Tidy`] — *would the next turn not fit, so should something be
///   tidied first?* `auto_compact` (the operator's switch) and the threshold answer
///   it, and the wall controller (`crate::wall`) stands it down while a folded base
///   is unmeasured or when every fold was measured still at the wall (`Stuck`).
/// * [`WallWhy::Refused`] — *a turn was just refused for context length, is there
///   a way forward?* The refusal is the evidence, so there is no threshold to
///   re-check and no flag to read: the provider measured the prompt against the
///   real window and said no.
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
    /// One entry per submission — so a turn that called three tools has four.
    pub metrics: Vec<TurnMetrics>,
    pub rounds: usize,
    pub tool_calls: usize,
    /// §5.7: the output was cut short but is usable. Never folded into success.
    pub truncated: bool,
}

impl Reply {
    /// D11's `f_keep` — `cached / what the previous turn left in the cache` — for
    /// every submission this reply cost.
    ///
    /// Shorter than `metrics` when a submission had no cached entry to measure
    /// against (a first turn, or a backend that skipped the check); those are
    /// dropped rather than reported as 0.0 or 1.0.
    pub fn f_keep(&self) -> Vec<f64> {
        self.metrics.iter().filter_map(|m| m.f_keep()).collect()
    }

    /// `f_sim` — cached over *this* prompt, per submission. Falls as the
    /// conversation grows; see `TurnMetrics::f_sim`. Never compare it to a
    /// `f_keep` threshold.
    pub fn f_sim(&self) -> Vec<f64> {
        self.metrics.iter().filter_map(|m| m.f_sim()).collect()
    }
}

/// A `LogSink` that also hands back the item ids it saw.
///
/// See point 1 in the module header. It forwards everything unchanged: a sink that
/// filtered would make the head's view depend on which daemon published it.
struct CapturingSink {
    inner: LogSink,
    ids: Vec<String>,
    /// The last `turn_id` this sink saw start.
    ///
    /// Read for §4.5: when a turn fails, the daemon has to publish a terminal event
    /// *for that turn*, and `HarnessError` does not carry an id — the failure is
    /// raised in several places, one of them before a turn id exists at all. Taken
    /// off the event on its way past, for the same reason the item ids are: the
    /// minting rule lives in the engine and a second copy of it goes stale.
    turn_id: Option<String>,
}

impl CapturingSink {
    fn new(hub: Arc<Hub>) -> Self {
        CapturingSink {
            inner: LogSink::new(hub),
            ids: Vec::new(),
            turn_id: None,
        }
    }

    fn take_ids(&mut self) -> Vec<String> {
        std::mem::take(&mut self.ids)
    }
}

impl EventSink for CapturingSink {
    fn emit(&mut self, event: TurnEvent) {
        match &event {
            TurnEvent::TranscriptAppended { item_id, .. } => self.ids.push(item_id.clone()),
            TurnEvent::TurnStarted { turn_id, .. } => self.turn_id = Some(turn_id.clone()),
            _ => {}
        }
        self.inner.emit(event);
    }
}

/// **Pair every id a sink captured with its row's body** — the free half of
/// `Harness::reconcile`, so the summary-turn helper (which cannot borrow `&self`,
/// see [`Harness::run_summary_turn`]) pairs rows by the same rule and cannot drift
/// from it.
///
/// The ids come off the events rather than being recomputed; see point 1 in the
/// module header. A count mismatch is a bug in the pairing, not something to paper
/// over, so it is announced as a warning on the same log the head reads.
fn reconcile_rows(
    hub: &Arc<Hub>,
    sink: &mut CapturingSink,
    items: &[TranscriptItem],
) -> Vec<String> {
    let ids = sink.take_ids();
    if ids.len() != items.len() {
        hub.publish(letibot_sessionlog::SessionEvent::Warning {
            code: "record_item_pairing".into(),
            detail: format!(
                "{} TranscriptAppended events for {} items; the head will show \
                 empty rows. This is a daemon bug, not a transport one.",
                ids.len(),
                items.len()
            ),

            compaction: None,
        });
    }
    for (id, item) in ids.iter().zip(items) {
        hub.record_item(id, item.clone());
    }
    ids
}

// ---------------------------------------------------------------------------
// The authorisation trail
// ---------------------------------------------------------------------------

/// One thing that was said in this session, with **who said it** recorded at the
/// moment it was appended.
struct Said {
    speaker: Speaker,
    /// **At most [`UTTERANCE_CHARS`] characters** — see [`TrailMirror::say`].
    text: String,
    /// It was longer than that when it was said, and says so — the flag is what
    /// `Utterance::clipped` reports, and it is recorded here rather than recomputed
    /// because the text this struct holds is already the clipped one.
    clipped: bool,
    /// Which turn of this session it belongs to.
    turn: u32,
    /// When it was said. `None` for a row rebuilt from the store — see
    /// [`TrailMirror`].
    at: Option<Instant>,
}

/// **The authorisation trail's source**, and the reason it is a mirror rather than
/// a walk over `session.items`.
///
/// `docs/boundary-and-adjudication.md` §2: a stateless command classifier cannot be
/// correct, because *"it is ok to use ssh key if i said ssh to that host"* — the
/// same command is authorised or not depending on what the operator just said. So
/// the adjudicator's input is a command **plus what authorised it**.
///
/// # Why the speaker cannot be inferred afterwards
///
/// Everything this session appends as a `TranscriptItem::User` looks identical in
/// the transcript: the operator's prompt, a head's steering, §5.7's salvage notice,
/// and the intent check's own *"you said you would X"*. Walking the transcript and
/// calling all of them the operator's words would let **the harness authorise
/// itself**, and an agent whose own text can authorise an action is the shape a
/// prompt injection would most like to take — which is exactly why
/// [`Speaker`] exists and why [`Speaker::Agent`] never authorises on its own.
///
/// The provenance is known only at the moment of appending, so it is recorded
/// there. This type is that record. It is **not** a second transcript: it holds
/// the user-side text and a count of everything else, and the count is the
/// denominator — *"0 of 41 messages"* is a measurement and *"0"* is not.
///
/// # Where `seconds_ago` comes from
///
/// From here. `letibot-transcript` does not stamp items, so
/// [`Utterance::seconds_ago`] is `None` unless a session loop supplies it — and
/// this is that session loop. A live utterance is stamped with an [`Instant`] when
/// it is appended. A row **rebuilt from the store** is not: nothing recorded when
/// it was said, and a reconstructed clock would be a guess presented as a
/// measurement. Those keep `None`, which reads as *not recorded* rather than as
/// *just now*, and their `turns_ago` is still exact.
#[derive(Default)]
pub struct TrailMirror {
    inner: Mutex<TrailInner>,
}

#[derive(Default)]
struct TrailInner {
    said: Vec<Said>,
    /// Every transcript item this session has, of any kind. The denominator.
    items: usize,
    /// The turn currently being served. 0 before the first one.
    turn: u32,
    /// **The agent's most recent statement of what it is doing**, for the guard to
    /// connect the operator's words to a call with. Never an authorisation, never
    /// citable — see [`letibot_tools::authorise::ModelBrief::agent_claim`].
    ///
    /// One line, replaced rather than accumulated: the question is what it is doing
    /// NOW, and a history of claims is a history the agent wrote.
    claim: Option<String>,
}

impl TrailMirror {
    fn lock(&self) -> std::sync::MutexGuard<'_, TrailInner> {
        // Session state, not a safety boundary: recovering a poisoned guard is
        // right, because losing the trail would turn a panic somewhere else into a
        // gate that suddenly cannot see what authorised anything.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A turn is starting. `turns_ago` is measured from here.
    fn begin_turn(&self) {
        self.lock().turn += 1;
    }

    /// **The denominator.** Every item this session appended, of any kind.
    ///
    /// Kept apart from [`TrailMirror::say`] because the two answer different
    /// questions and collapsing them is the defect `TrailProvenance` exists to
    /// stop: *"I looked at 41 messages and 0 were the operator's"* is a
    /// measurement, and *"0"* is not.
    fn note_items(&self, n: usize) {
        self.lock().items += n;
    }

    /// Record one thing that was said, with the provenance the caller knows and
    /// nobody else can recover.
    ///
    /// **Two caps, and both were already the only values anything read.** The trail
    /// holds at most [`TRAIL_SCAN`] utterances and at most [`UTTERANCE_CHARS`]
    /// characters of each: `trail()` takes the last sixty and clips each to six
    /// hundred, so everything this drops was memory no reader could reach. It is a
    /// memory bound and not a behaviour change — and it is what makes
    /// `crate::footprint`'s trail ceiling a bound the code holds rather than a wish.
    fn say(&self, speaker: Speaker, text: &str, at: Option<Instant>) {
        if text.trim().is_empty() {
            return;
        }
        let clipped = text.chars().count() > UTTERANCE_CHARS;
        let stored: String = if clipped {
            text.chars().take(UTTERANCE_CHARS).collect()
        } else {
            text.to_string()
        };
        let mut g = self.lock();
        let turn = g.turn;
        g.said.push(Said {
            speaker,
            text: stored,
            clipped,
            turn,
            at,
        });
        Self::keep_the_last_scan(&mut g.said);
    }

    /// Drop everything past the [`TRAIL_SCAN`] the walk can reach.
    ///
    /// One statement, in one place, because the two writers (`say` and `seed`) must not
    /// disagree about how long the trail is: a seeded trail that kept 500 rows and a live
    /// one that kept 60 would be two bounds over one buffer.
    fn keep_the_last_scan(said: &mut Vec<Said>) {
        if said.len() > TRAIL_SCAN {
            let drop = said.len() - TRAIL_SCAN;
            said.drain(..drop);
        }
    }

    /// **What the trail holds**, in bytes of text and in utterances — the third term of a
    /// hub's [`Footprint`](letibot_sessionlog::hub::Footprint), read live because this is
    /// already shared behind its own lock.
    pub(crate) fn held(&self) -> (usize, usize) {
        let g = self.lock();
        (g.said.iter().map(|s| s.text.len()).sum(), g.said.len())
    }

    /// Seed from a transcript rebuilt out of the store.
    ///
    /// **The speaker comes off the row** — R42. Until 2026-10-05 this called every
    /// `User` row the operator's, on a reading that was honest when it was written:
    /// *"the store does not record which of them the harness injected"*. Then R42 added
    /// `TranscriptItem::User::speaker` and nothing came back here to read it, so the
    /// claim outlived the code that made it true.
    ///
    /// What it cost: a resumed session put its own job settlements, salvage notices and
    /// intent checks into the authorisation trail **as the operator's utterances**.
    /// Measured on the live corpus, 280 of the 715 briefs the oracle answered carried a
    /// `[job] a job you backgrounded has ended` notice rendered under `[operator · …]`,
    /// numbered, and citable as `ALLOW <n>`.
    ///
    /// The guard that was defeated is built and correct:
    /// [`AuthorisationTrail::cited_operator_words`] already refuses an ALLOW citing an
    /// agent line — *"the agent authorising itself… the exact move the trail exists to
    /// make impossible"* — and it cannot refuse what arrives relabelled. So the fix
    /// belongs here rather than there: the check was never wrong, its input was.
    ///
    /// A row written before the field existed deserialises as `Operator`
    /// (`#[serde(default)]`), which is what a head already drew for it, so an old
    /// transcript reads exactly as it did.
    fn seed(&self, items: &[TranscriptItem]) {
        let mut g = self.lock();
        g.items = items.len();
        g.said.clear();
        for (i, item) in items.iter().enumerate() {
            if let TranscriptItem::User { parts, speaker } = item {
                let text: String = parts
                    .iter()
                    .filter_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.trim().is_empty() {
                    continue;
                }
                // The same two caps a live utterance gets — see `say`. A resumed session's
                // rows are the same text by the same rule, so a seeded trail and a live one
                // are one bound.
                let clipped = text.chars().count() > UTTERANCE_CHARS;
                let text: String = if clipped {
                    text.chars().take(UTTERANCE_CHARS).collect()
                } else {
                    text
                };
                g.said.push(Said {
                    // **The two `Speaker` enums meet here and nowhere else**: the
                    // row's (`letibot_transcript`, two variants) and the trail's
                    // (`letibot_tools::authorise`, which also has `Tool`). Matched
                    // inline rather than behind a `From`, so the one place they touch
                    // is the one place a reader has to look for the mapping.
                    speaker: match speaker {
                        letibot_transcript::Speaker::Operator => Speaker::Operator,
                        letibot_transcript::Speaker::Agent => Speaker::Agent,
                    },
                    text,
                    clipped,
                    // Item index as a stand-in turn: the ordering is right and the
                    // distances are monotone, which is what `turns_ago` is read for.
                    turn: i as u32,
                    at: None,
                });
                Self::keep_the_last_scan(&mut g.said);
            }
        }
        g.turn = items.len() as u32;
    }

    /// What the agent last said it was doing, bounded the way an utterance is.
    fn claim(&self) -> Option<String> {
        self.lock().claim.clone()
    }

    /// Record it. Called with the assistant's own prose for a turn — the sentence
    /// before the tool calls, which is where a model says what it is about to do.
    fn claims(&self, text: &str) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        let mut g = self.lock();
        g.claim = Some(t.chars().take(UTTERANCE_CHARS).collect());
    }

    /// **The trail, walked backwards.** What
    /// [`AdjudicatedGate::with_trail_source`] installs.
    pub fn trail(&self) -> AuthorisationTrail {
        let g = self.lock();
        let now = Instant::now();
        let scanned = g.said.len().min(TRAIL_SCAN);
        let mut utterances: Vec<Utterance> = Vec::new();
        for said in g.said.iter().rev().take(TRAIL_SCAN) {
            if utterances.len() >= TRAIL_UTTERANCES {
                break;
            }
            // The text is stored already clipped, so `clipped` comes off the record and the
            // comparison is kept as the belt to that pair of braces: a row this struct did not
            // write (there is none today) would still read correctly.
            let clipped = said.clipped || said.text.chars().count() > UTTERANCE_CHARS;
            utterances.push(Utterance {
                speaker: said.speaker,
                text: if clipped {
                    said.text.chars().take(UTTERANCE_CHARS).collect()
                } else {
                    said.text.clone()
                },
                clipped,
                turns_ago: g.turn.saturating_sub(said.turn),
                seconds_ago: said.at.map(|t| now.duration_since(t).as_secs()),
            });
        }
        // The denominator is what was **looked at**, not what came back. Deriving
        // it from the result would collapse "I looked at 41 messages and 0 were the
        // operator's" into "I looked at 0", which is the whole distinction
        // `TrailProvenance` exists to keep.
        AuthorisationTrail::from_messages(utterances, scanned.max(g.items.min(TRAIL_SCAN)))
    }
}

// ---------------------------------------------------------------------------
// A question the model asked, and the answer a person gave it
// ---------------------------------------------------------------------------

/// **The tool whose call is an exchange with a person** rather than a piece of work.
///
/// Spelled here and not imported from `letibot-tools`, because what the name means at
/// THIS layer is *the one call whose result carries a person's own utterance* — a fact
/// about the transcript, not about the tool's schema.
const ASK_USER_QUESTION: &str = "ask_user_question";

/// **The person's own words, exactly** — what their row carries, or `None` when there is
/// no honest way to state them.
///
/// The brief this answers is the one the `!` line's own row keeps: *their words, verbatim*.
/// So the parts are read off the ANSWER and nothing of ours is prepended — a citable
/// utterance with a harness sentence glued to its front is a citable utterance of the
/// wrong thing.
///
/// | what they did | what the row says |
/// |---|---|
/// | chose an option | the option's own text |
/// | chose, and qualified it | the option's text, then their note, on its own line |
/// | typed an answer | what they typed |
/// | abstained | `abstain` — the act, which is the only word for it the wire carries |
///
/// `None` is *an answer with nothing of the person's in it*: an option index the question
/// never offered. The tool refuses one of those (`QuestionAnswer::validate`), and the round
/// loop writes no rows for a result the tool did not accept — so this arm is the belt to
/// that pair of braces and not a state a session reaches.
fn operator_answer_text(asked: &crate::answers::Asked) -> Option<String> {
    let a = &asked.answer;
    if a.abstain {
        return Some("abstain".to_string());
    }
    let mut said: Vec<String> = Vec::new();
    if let Some(i) = a.option {
        said.push(asked.question.options.get(i)?.clone());
    }
    if let Some(n) = a.note.as_deref().filter(|n| !n.trim().is_empty()) {
        said.push(n.trim().to_string());
    }
    if let Some(f) = a.free.as_deref().filter(|f| !f.trim().is_empty()) {
        said.push(f.trim().to_string());
    }
    if said.is_empty() {
        return None;
    }
    Some(said.join("\n"))
}

/// **The question, as the SESSION's own row** — never the operator's.
///
/// The trail reads every `User` row with `speaker: Operator` as words of theirs that an
/// action may cite, so a question put in their mouth would be a question the harness could
/// quote back as authorisation for the answer to it. This row is `Speaker::Agent` for that
/// reason and for the other one: the question is the model's, and a head drawing it as the
/// person's would be showing the reader the wrong half of the exchange as theirs.
///
/// **The options are a LIST, one per line** (R90.24): `options offered:` and then `- OPTION` for
/// each, an option's own line breaks folded to spaces. They used to be joined with ` | ` on one
/// line, and an option holding ` | ` came back as two. The head reads either shape
/// (`asked_card::question_of`), so stored rows still parse.
fn question_row_text(asked: &crate::answers::Asked) -> String {
    let q = &asked.question;
    let mut said = format!("asked: {}", q.text.trim());
    if !q.options.is_empty() {
        said.push_str("\noptions offered:");
        for o in &q.options {
            said.push_str("\n- ");
            said.push_str(&o.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    said
}

/// **The two rows the person's half of a question makes**, as a free function so the shape
/// can be tested without a daemon, a model or a socket — the same reason `abandoned_calls`
/// is one.
///
/// The order is the question and then the answer, which is how a reader walks them. Where
/// they land relative to the call's own result is the round loop's business, and it is not
/// this order: see `Harness::settle_asked`.
fn asked_rows(asked: &crate::answers::Asked, said: &str) -> [TranscriptItem; 2] {
    [
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text {
                text: question_row_text(asked),
            }],
        },
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: said.to_string(),
            }],
        },
    ]
}

// ---------------------------------------------------------------------------
// Surfacing a denial
// ---------------------------------------------------------------------------

/// **Every refusal, onto the session's own log, the moment it is decided.**
///
/// `docs/boundary-and-adjudication.md` §4b. Not at turn end and not on request: the
/// whole defect is that the operator learns about a denial by noticing a task that
/// stopped, infers nothing was decided, and watches the model try a variant.
///
/// It publishes rather than returning, because the gate calls it from inside the
/// tool path and the operator may be somewhere else entirely. The log is the one
/// place both a live head and a late one look.
/// Public because it is the *only* implementation of `DenialSink` that reaches a
/// person, and a seam whose one real implementation is private is a seam nobody
/// else can check.
pub struct HubDenials {
    hub: Arc<Hub>,
}

impl HubDenials {
    pub fn new(hub: Arc<Hub>) -> Self {
        HubDenials { hub }
    }
}

/// **Where a gate admission goes** (D16) — the channel in the other direction from
/// [`HubDenials`], and the reason it is a seam rather than a `publish`.
///
/// An admission is not a denial and must not be drawn as one: nothing was refused, the model
/// has its result, and there is no card to answer. What the operator is owed is *to be told it
/// happened in their session*, which is a transcript row — and a transcript row is the
/// harness's to write, on the harness's thread. So this collects the sentence and
/// [`Harness::flush_gate_admissions`] says it at a round boundary.
pub struct HubAdmissions {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl HubAdmissions {
    pub fn new(lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Self {
        HubAdmissions { lines }
    }
}

impl letibot_tools::adjudicate::AdmissionSink for HubAdmissions {
    fn admitted(&self, notice: &letibot_tools::adjudicate::AdmissionNotice) {
        // **One line, and the command is the part a reader scans for.** The reason is in the
        // audit row (`basis`); what a reader of the transcript needs is *which command ran
        // without me* and *which rule said so*.
        self.lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!(
                "admitted without asking — `{}` {} · inside the measured boundary ({}, \
                 {}), D16 · the row and `/gate` carry the full reason",
                notice.tool,
                notice
                    .summary
                    .trim_start_matches(&format!("`{}` ", notice.tool)),
                notice.boundary,
                notice.mode
            ));
    }
}

impl DenialSink for HubDenials {
    fn denied(&self, notice: &DenialNotice) {
        let (repeat_count, breaker_open) = match notice.repeat {
            BreakerState::Closed => (1u32, false),
            BreakerState::Repeat { consecutive } => (consecutive.max(1) as u32, false),
            BreakerState::Open { consecutive } => (consecutive.max(1) as u32, true),
        };
        self.hub.publish(SessionEvent::DenialRaised {
            request_id: notice.request_id.clone(),
            turn_id: notice.turn_id.clone(),
            call_id: notice.call_id.clone(),
            tool: notice.tool.clone(),
            summary: notice.summary.clone(),
            baseline: notice.baseline.clone(),
            by: notice.by.clone(),
            basis: notice.basis.clone(),
            tier: notice.tier.to_string(),
            outcome: notice.outcome.to_string(),
            repeat_count,
            breaker_open,
            grant: notice.grant.clone(),
        });
    }
}

// ---------------------------------------------------------------------------
// Steering
// ---------------------------------------------------------------------------

/// Steering from the head socket (§5.8), from the intent check, from a fired
/// monitor, and from a background job of this session's own that has ended.
///
/// A `Prompt` that arrives while the model is generating is a correction, and it
/// goes in at the next step boundary. An `Interrupt` is the urgent form: it stops
/// generation at the next token and the partial output is kept.
///
/// The hub's queue is drained here *only while a turn is running*; between turns
/// the daemon's own worker owns it. One drainer at a time, by construction of who
/// is executing.
///
/// # Four sources, one queue, and the speaker is recorded as they are drained
///
/// The step boundary is the only place any of the four can be injected, so they
/// share this source rather than each growing a path into the engine. What they do
/// **not** share is provenance: a head's prompt is the operator, and the intent
/// check, a monitor firing and a settlement are this harness talking to itself. That
/// distinction is recorded here, at the moment the message is handed over, because after
/// the engine appends it they are four identical `User` items. See [`TrailMirror`].
///
/// # Why a settlement is here at all, when the session has a wake
///
/// For a **root** it is not: [`Harness::wake`] is its door, and
/// [`JobWatchers::take_mid_turn_completions`] returns nothing for it — a second
/// delivery path for the R7 turn would be exactly the kind of duplicate this tree
/// keeps finding. A **child** has a between-turn wake too, and since 2026-10-06 it is one
/// that ARRIVES: the ring names the child, `Sessions::wake` cannot run its turn (`open`
/// holds no harness for it), so the daemon hands the wake to the thread that does
/// ([`letibot_sessionlog::hub::Hub::wake_its_own_reader`], answered by `serve_child`) and
/// the settlement becomes a turn of the child's own. What that door cannot reach is a turn
/// already RUNNING — the child's serving thread is inside it — and the round boundary is
/// the only place a turn in flight can be told anything. This is that place.
///
/// That guard is the set's, not this struct's: it is the same question — *is this session
/// the top of its tree* — and it is answered where the session ids are.
///
/// **Since R79 the question here is a different one, and it is asked of roots and children
/// alike: does the work wait on this result?** Only what it does — a row's `when`/`needs`, a
/// wait, `wake: "now"`, the failure of one of those — comes in at a round boundary; everything
/// else is held for the next turn boundary or the starvation cap ([`Deliveries`],
/// `crate::delivery`). One queue and one sort still mean one delivery: the wake that follows
/// finds the result taken or held, never untold.
pub struct HubSteering {
    hub: Arc<Hub>,
    /// Where the speaker of each injected message is recorded. `None` in a test
    /// that only wants the head's queue.
    trail: Option<Arc<TrailMirror>>,
    /// Text this harness generated for the next step boundary: the intent diff's
    /// findings, and a monitor that fired mid-turn.
    injected: Arc<Mutex<VecDeque<String>>>,
    /// This session's monitors, when it has an exec backend.
    monitors: Option<Arc<Monitors>>,
    /// How many monitors had settled when this source last looked. Shared with the
    /// harness so a firing is delivered exactly once, whether it is picked up
    /// mid-turn here or between turns by [`Harness::wake`].
    monitor_cursor: Arc<AtomicUsize>,
    /// **This session's results, delivered by R79's rules** — the immediates at every round
    /// boundary, the held batch only past the starvation cap. `None` in a test that has no
    /// daemon behind it.
    deliveries: Option<Deliveries>,
    /// **This session's children** — the tree's downward edge. An `Interrupt` that reaches a
    /// running turn IS this session being stopped, and a supervisor stops its children before
    /// it goes (see [`HarnessTaskRunner`]). `None` in a test with no session behind it.
    subagents: Option<Arc<dyn letibot_tools::builtins::task::TaskRunner>>,
    /// Messages handed back by a failed turn, drained before the hub's queue so the
    /// next poll finds them first. See [`SteeringSource::give_back`].
    ///
    /// **Shared with the harness (R91.1)**, not owned by this value: a `HubSteering` is
    /// built per round, and one that owned what it was handed back took it with it when
    /// the round's retry, or the turn, let it go — an operator follow-up typed mid-stream
    /// and then a provider drop or an esc-esc, R79 immediates already marked delivered,
    /// a monitor firing whose cursor had moved. In the harness's queue they survive into
    /// the retried round, the next round, and — after a failed turn — the next turn.
    held: Arc<Mutex<VecDeque<SteeringMessage>>>,
    /// **This session's board**, for the one command a running turn applies to it: the
    /// operator's answer to a row's question (`CommandKind::AnswerTodo`), which reaches the model
    /// at the next step boundary rather than after the turn. `None` in a test with no board.
    todos: Option<Arc<TodoBoard>>,
    /// **This session's own parent, if it has one** — the one fact
    /// [`message_from_a_session`] needs to word a message correctly, and the reason it is here
    /// rather than read from the hub: a message says *"your parent session"* only when the
    /// sender IS this session's parent, and the between-turns door reads the same field off the
    /// harness's config. Two doors, one answer — see the `Message` arm.
    parent: Option<String>,
}

impl HubSteering {
    pub fn new(hub: Arc<Hub>) -> Self {
        HubSteering {
            hub,
            trail: None,
            injected: Arc::new(Mutex::new(VecDeque::new())),
            monitors: None,
            monitor_cursor: Arc::new(AtomicUsize::new(0)),
            deliveries: None,
            subagents: None,
            held: Default::default(),
            todos: None,
            parent: None,
        }
    }
}

/// The warning code for a session whose workspace no longer exists (R92.24).
const WORKSPACE_GONE: &str = "workspace_gone";

/// **The warning code for a session that asked for a boundary and got a whole-host shell**
/// (TODO R98).
///
/// Said once, at open, on the session's own hub and in the daemon's log. It exists because the
/// consequence is invisible from the inside: D16 admits a command without a card only *inside a
/// measured boundary*, so a session without one asks the operator about every `cargo test` — and
/// an operator who is not told that would read the cards as the gate being broken.
///
/// **macOS only**, and that is the shape of the rule rather than an accident: on Linux a boundary
/// that cannot be built is a refusal, so there is no unconfined session to warn about
/// (`docs/boundary-and-adjudication.md` §4k). The one arm that publishes this is the macOS
/// fallback in `Harness::open`, which is behind the same `cfg`.
#[cfg(target_os = "macos")]
const BOUNDARY_ABSENT: &str = "boundary_absent";

/// The warning code for a session whose tools run on another machine — where its tree is,
/// the branch that comes back, and what stayed here (`docs/remote-hosts.md`).
const REMOTE_PLACEMENT: &str = "remote_placement";

/// **"workspace gone", in one sentence** — said when a session opens or wakes on a workspace that
/// no longer exists (R92.24).
fn workspace_gone(workspace: &std::path::Path) -> String {
    format!(
        "workspace gone: `{}` no longer exists (a retired worktree, or a checkout moved or \
         removed), so nothing this session runs can work there",
        workspace.display()
    )
}

/// A monitor firing, as the sentence the model is given — or `None` when nothing in `fired` is
/// news to it.
///
/// It reports **why it fired**, not that it did — T24 requirement 3 — and (R93.C8) one line per
/// watch in its own words: `[monitor] \`name\` fired: why.` or `[monitor] \`name\` expired
/// without firing.` The model's OWN `retire` and an owner ending are not announced at all: the
/// first is the model's act one call ago (and a wake spent telling it so was a turn for nothing),
/// the second is the scope it declared the watch under going away. Both are still recorded
/// (`Deliveries::log_firings`); only the sentence is spared. The old head ("N watch(es) fired")
/// called an expiry a firing, leaked the declaring turn id, and closed with the same aphorism on
/// every notice.
fn monitor_notice(fired: &[letibot_tools::exec::monitor::Firing]) -> Option<String> {
    use letibot_tools::exec::monitor::Fired;
    let lines: Vec<String> = fired
        .iter()
        .filter_map(|f| match &f.fired {
            Fired::Fired { why, .. } => Some(format!("[monitor] `{}` fired: {why}.", f.name)),
            Fired::Expired { .. } => {
                Some(format!("[monitor] `{}` expired without firing.", f.name))
            }
            Fired::Cancelled { by, .. } if by != MODEL_RETIRE => {
                Some(format!("[monitor] `{}` was retired: {by}.", f.name))
            }
            Fired::Cancelled { .. } | Fired::OwnerEnded { .. } => None,
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// **The firings `monitor_notice` was composed from**, as the facts the `notice` hook is handed
/// (`docs/plugins-plan.md` §4.3).
///
/// The point of handing them over is that a layer can build a different sentence out of the same
/// news — *the pull request the firing is about is in conflict* is not a rewording of this text,
/// it is a sentence about these facts.
fn monitor_facts(fired: &[letibot_tools::exec::monitor::Firing]) -> serde_json::Value {
    use letibot_tools::exec::monitor::Fired;
    let rows: Vec<serde_json::Value> = fired
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "watch": f.watch,
                "ended": f.fired.word(),
                "why": match &f.fired {
                    Fired::Fired { why, .. } => Some(why.clone()),
                    Fired::Expired { .. } => None,
                    Fired::Cancelled { by, .. } => Some(by.clone()),
                    Fired::OwnerEnded { .. } => None,
                },
            })
        })
        .collect();
    serde_json::json!({"fired": rows})
}

/// **How many ended monitors the pane lists** — the newest. A session that ran a repeating watch
/// all day would otherwise ship its whole history with every job-table refill.
pub const MONITORS_ENDED_SHOWN: usize = 20;

/// **What reaps a monitor, as a person says it** — `session`, `turn`, or a named scope's own name;
/// the scope's id (`session.s0`) is the daemon's, not the reader's.
fn owner_words(s: &letibot_tools::exec::scope::ScopeId) -> String {
    use letibot_tools::exec::scope::ScopeKind;
    match s.kind {
        ScopeKind::Session => "session".into(),
        ScopeKind::Turn => "turn".into(),
        ScopeKind::Explicit => format!("scope `{}`", s.name),
    }
}

/// **Who armed a monitor, as a person says it** — `turn pr-watch` was the harness on a
/// `gh pr create`, `turn restart` a restart re-arming its stored call; a model's own turn is
/// said as it is.
fn declared_words(by: &str) -> String {
    match by {
        "turn pr-watch" => "the harness, when the pull request was opened".into(),
        "turn restart" => "a restart, from the call that armed it".into(),
        other => other.to_string(),
    }
}

/// [`Harness::monitor_entries`] over its inputs, so the rows are assertable without a session.
pub fn monitor_entries_of(
    live: &[std::sync::Arc<letibot_tools::exec::monitor::Monitor>],
    firings: &[letibot_tools::exec::monitor::Firing],
    now_ms: u64,
) -> Vec<letibot_sessionlog::protocol::MonitorEntry> {
    use letibot_tools::exec::monitor::Fired;
    let fired_count = |name: &str| {
        firings
            .iter()
            .filter(|f| f.name == name && matches!(f.fired, Fired::Fired { .. }))
            .count() as u32
    };
    let mut out: Vec<letibot_sessionlog::protocol::MonitorEntry> = live
        .iter()
        .map(|m| letibot_sessionlog::protocol::MonitorEntry {
            name: m.name.clone(),
            watching: m.watch.describe(),
            owner: owner_words(&m.owner),
            declared_by: declared_words(&m.declared_by),
            repeat: m.repeats(),
            live: true,
            ends_at_ms: now_ms + m.remaining().map_or(0, |r| r.as_millis() as u64),
            fired: fired_count(&m.name),
            ended: String::new(),
        })
        .collect();
    // The last event of each name that is no longer live is how it ended; a name armed again
    // later is live and listed above, and its earlier end is history the pane does not repeat.
    let mut ended: Vec<&letibot_tools::exec::monitor::Firing> = Vec::new();
    for f in firings.iter().rev() {
        if live.iter().any(|m| m.name == f.name) || ended.iter().any(|e| e.name == f.name) {
            continue;
        }
        // A repeating monitor's firing is not its end; its end is the record that took it.
        ended.push(f);
        if ended.len() == MONITORS_ENDED_SHOWN {
            break;
        }
    }
    ended.reverse();
    out.extend(
        ended
            .into_iter()
            .map(|f| letibot_sessionlog::protocol::MonitorEntry {
                name: f.name.clone(),
                watching: f.watch.clone(),
                owner: String::new(),
                declared_by: declared_words(&f.declared_by),
                repeat: false,
                live: false,
                ends_at_ms: 0,
                fired: fired_count(&f.name),
                ended: f.fired.word(),
            }),
    );
    out
}

#[cfg(test)]
mod monitor_rows {
    use super::*;
    use letibot_tools::exec::monitor::{DEFAULT_TTL, Monitors, Watch};
    use letibot_tools::exec::scope::{ScopeId, ScopeKind};

    /// **The monitors pane's rows** — a live repeating watch is one row with its firing count and
    /// its deadline; a one-shot that fired and one the operator retired are ended rows saying how,
    /// in the order they ended.
    #[test]
    fn live_monitors_come_first_and_ended_ones_say_how_they_ended() {
        let dir = std::env::temp_dir().join(format!("letibot-monrows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let scope = ScopeId {
            kind: ScopeKind::Session,
            name: "s".into(),
            path: dir.clone(),
        };
        let ms = Arc::new(Monitors::new());
        let watch = |f: &str| Watch::Path(dir.join(f));
        ms.declare(
            "tick",
            scope.clone(),
            watch("a"),
            None,
            "turn 1",
            DEFAULT_TTL,
            true,
        )
        .unwrap();
        ms.declare(
            "once",
            scope.clone(),
            watch("b"),
            None,
            "turn 2",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        ms.declare(
            "gone",
            scope.clone(),
            watch("c"),
            None,
            "turn 3",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        std::fs::write(dir.join("a"), b"1").unwrap();
        std::fs::write(dir.join("b"), b"1").unwrap();
        assert_eq!(ms.tick().len(), 2, "both watched files changed");
        ms.retire("gone", OPERATOR_RETIRE).unwrap();

        let now = 1_000_000;
        let rows = monitor_entries_of(&ms.list(), &ms.firings(), now);
        let names: Vec<(&str, bool)> = rows.iter().map(|r| (r.name.as_str(), r.live)).collect();
        assert_eq!(
            names,
            vec![("tick", true), ("once", false), ("gone", false)]
        );
        let tick = &rows[0];
        assert!(tick.repeat && tick.fired == 1, "{tick:?}");
        assert!(
            tick.ends_at_ms > now && tick.ends_at_ms <= now + 300_000,
            "{tick:?}"
        );
        assert!(
            tick.ended.is_empty() && tick.watching.contains('a'),
            "{tick:?}"
        );
        assert!(rows[1].ended.starts_with("FIRED"), "{:?}", rows[1]);
        assert_eq!(rows[1].fired, 1);
        assert!(
            rows[2].ended.starts_with("retired by the operator"),
            "{:?}",
            rows[2]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Who `monitor`'s own `retire` op says retired a watch (`builtins::monitor`) — the model's act,
/// which [`monitor_notice`] does not announce back to it.
const MODEL_RETIRE: &str = "the model";

/// Who a retire from the monitors pane says retired a watch — announced to the model, unlike its
/// own ([`MODEL_RETIRE`]).
const OPERATOR_RETIRE: &str = "the operator";

/// **The completion of a background job, as the sentence the model is handed (R7).**
///
/// The other half of [`monitor_notice`], and the same shape: a labelled notice, in the
/// harness's own voice, submitted as a turn. What it must carry is what the acceptance
/// criterion names — the job, its command, how it ended, and where its output is — and
/// what it must **not** do is inline the output: `produced` is a byte count and the words
/// are one `job_output` away, so a build log of 27,000 lines never enters the context
/// unbidden.
///
/// The closing sentence is the other half of R7, and it is a promise rather than a
/// footnote: *you do not need to wait for this.* The tool text is what taught the model
/// that waiting was the only way to learn a result; see `bash`'s backgrounded result and
/// `job_output`'s empty-job branch for the strings that did the teaching.
///
/// **And the job is named the way every other sentence names it.** The operator reads this
/// line in the transcript, and the complaint that put this here was theirs, verbatim:
/// *"j57 doesnt ringe the bell for me"* — a counter is not a name. So the bullet carries
/// [`letibot_tools::exec::job_label`]'s spelling, `release-build · j65`, which is the same
/// string `bash`'s backgrounded result printed when the job was started and the same one
/// `/job` draws; a job nobody named is still just `j65`, byte for byte, because the label's
/// fallback IS the id. The name is on the settlement ([`JobCompletion::slug`]) rather than
/// looked up here, because by the time this runs the job has usually been reaped — the same
/// reason [`JobCompletion::command`] is carried.
///
/// **`mine` is the ownership flag, and it is not a politeness.** `true` is the session that
/// backgrounded the job — the only one that can read it, since `job_output` asks *this*
/// session's host. `false` is the level the settlement was handed up to because the session
/// that started it had stopped ([`JobWatchers::stop`]): the owning session is named on the
/// line instead, and the closing sentence does **not** send this reader to `job_output`,
/// because for it that door does not open.
fn completion_notice(done: &[JobCompletion], mine: bool) -> String {
    let mut s = String::from("[job] ");
    match (mine, done.len() == 1) {
        (true, true) => s.push_str("a job you backgrounded has ended:\n"),
        (true, false) => s.push_str(&format!(
            "{} jobs you backgrounded have ended:\n",
            done.len()
        )),
        (false, true) => s.push_str("a job a session below this one started has ended:\n"),
        (false, false) => s.push_str(&format!(
            "{} jobs a session below this one started have ended:\n",
            done.len()
        )),
    }
    for c in done {
        // R93.C3: no byte count (a number with nothing to do), and the command only for a job
        // ANOTHER session started — the model wrote its own one call ago, and echoing it back in
        // full was most of the notice.
        let whose = if mine {
            String::new()
        } else {
            let command = if c.command.is_empty() {
                "command not recorded"
            } else {
                c.command.as_str()
            };
            format!(": {command} (started by `{}`)", c.owner)
        };
        s.push_str(&format!(
            "  - `{}` {} after {}{}\n",
            letibot_tools::exec::job_label(c.slug.as_deref(), &c.job),
            c.state,
            human_secs(c.elapsed_ms),
            whose,
        ));
        if let Some(m) = &c.memory {
            s.push_str(&format!("    memory: {m}\n"));
        }
        // **The one job whose result IS its output** — a fanout's per-host table, which
        // [`letibot_tools::exec::ProcessHost::settlement_note`] answers with and nothing else does.
        // Indented under the row it belongs to, and carried in `detail` rather than read from a
        // stream, because the job has usually been reaped by the time this is built.
        if !c.detail.is_empty() {
            for line in c.detail.lines() {
                s.push_str(&format!("    {line}\n"));
            }
        }
    }
    if mine {
        // R93.C1: the closing named `job_wait` on every completion, said "you do not need to wait"
        // to a model that had just waited, and gave `job="…"` for an id it knew. It names the door.
        s.push_str(&format!("{JOB_CLOSING}{}.", handles_as("job", done)));
    } else {
        s.push_str(
            "This session is told because the one that started it cannot be: it has stopped and \
             will not run again, and the job's output went with it. There is nothing to collect \
             and nothing to wait for — this is the fact of the ending and no more.",
        );
    }
    s
}

/// **The same sentence for a subagent, and the same rule behind it.**
///
/// A subagent settles through the job channel because it *is* one — the operator's
/// ruling — and this is only the wording, not a second mechanism. What differs is the
/// verb: a subagent has no `job_output` to read, it has `task_result`, and its answer is
/// the thing the model was waiting for rather than a stream it has to go and fetch. So
/// the notice carries the child's first line as well as naming how to collect the whole
/// of it, which is what makes the wake actionable rather than a nudge.
///
/// **And `mine` is the same flag as the job side's**, with one difference worth stating:
/// a handed-up child is still collectable here — `task_result` reads the tree's handle list
/// (`Parts::tree_slots`, R58), which every level of a tree shares — so the closing sentence
/// keeps its promise where the job side's cannot.
fn subagent_notice(done: &[JobCompletion], mine: bool) -> String {
    let mut s = String::from("[task] ");
    match (mine, done.len() == 1) {
        (true, true) => s.push_str("a subagent you started has finished:\n"),
        (true, false) => s.push_str(&format!(
            "{} subagents you started have finished:\n",
            done.len()
        )),
        (false, true) => s.push_str(
            "a subagent a session below this one started has \
             finished:\n",
        ),
        (false, false) => s.push_str(&format!(
            "{} subagents a session below this one started have finished:\n",
            done.len()
        )),
    }
    for c in done {
        let whose = if mine {
            String::new()
        } else {
            format!(" (started by `{}`)", c.owner)
        };
        // **The child by its name**, `fix-login · s-…-sub-3` — the job rule, and the same
        // spelling `task` printed when it started the child. A child nobody named (a settlement
        // from a daemon before names) is its bare handle, byte for byte what this always said.
        let label = letibot_tools::builtins::task::subagent_label(c.slug.as_deref(), &c.job);
        if c.detail.is_empty() {
            s.push_str(&format!("  - `{label}` {}{}\n", c.state, whose));
        } else {
            s.push_str(&format!(
                "  - `{label}` {}: {}{}\n",
                c.state, c.detail, whose
            ));
        }
    }
    if mine {
        // R93.C2: "calling `task_result` would only hold you … Read what it said with
        // `task_result`" said don't and do in one breath, with `task="…"` for a known handle.
        s.push_str(&format!("{TASK_CLOSING}{}.", handles_as("task", done)));
    } else {
        s.push_str(&format!(
            "This session is told because the one that started it cannot be: it has stopped and \
             will not run again. The handle is in this tree, so the whole answer is still there: \
             `task_result` {}.",
            handles_as("task", done)
        ));
    }
    s
}

/// **The closing a `[job]` notice for this session's own jobs ends with**, up to its handles — the
/// words the head's fold and job card recognise (`tui`'s `PROMISES` and `CLOSINGS`).
pub const JOB_CLOSING: &str = "Output: `job_output` ";
/// The same for a `[task]` notice of this session's own children.
pub const TASK_CLOSING: &str = "Full answer: `task_result` ";

/// `job="j1", job="j2"` — the real handles, never a placeholder.
fn handles_as(field: &str, done: &[JobCompletion]) -> String {
    done.iter()
        .map(|c| format!("{field}=\"{}\"", c.job))
        .collect::<Vec<_>>()
        .join(", ")
}
/// **One channel, two nouns: the sentences a settlement produces, in one place.**
///
/// A job and a subagent arrive through the same queue and ring the same bell — see
/// [`JobWatchers::watch`] — and they are told apart **here** rather than in two queues, because two
/// queues would be the second mechanism the ruling avoided. The split is only for wording: a job is
/// read with `job_output` and a child with `task_result`.
///
/// **And one more split, which is ownership rather than wording — but it is wording that is
/// wrong without it.** A settlement is drained by the session that started it; the one
/// exception is a settlement whose own session stopped before it could be drained, which
/// [`JobWatchers::stop`] hands up a level so that it is not read by nobody. That
/// completion is not this session's, and `me` is what tells the two apart: `owner != me`
/// means *say whose it was*, do not say *you backgrounded*.
///
/// **Why it is a function rather than four lines inside [`Harness::wake`].** This is the one part of
/// the wake that decides anything, and `wake()` cannot be reached by a test: it needs a live
/// `Harness`, which this crate's tests build only where an apparatus (a vocabulary, a store) is
/// present. **A decision exercisable only by standing up a whole session is a decision nothing
/// asserts** — which is how *"nothing fired"* could be reported without CI disagreeing about whether
/// the notice could fire at all. So the partition is pure and has pure tests, and `wake()` is left
/// with the part that is not a decision: taking the queue and submitting what this returns.
///
/// An empty queue returns nothing, and that is load-bearing rather than tidy: `wake()` returns
/// `Ok(None)` on an empty result rather than running a turn with nothing to say.
fn completion_notices(done: Vec<JobCompletion>, me: &str, rows: &[TodoItem]) -> Vec<String> {
    // **Mine first**, then what was handed up — so a session reading one wake reads its
    // own settlements before its children's, and the order of the two groups cannot be
    // mistaken for one list.
    let (theirs, mine): (Vec<JobCompletion>, Vec<JobCompletion>) =
        done.into_iter().partition(|c| c.owner != me);
    let mut out = Vec::new();
    for (batch, mine) in [(mine, true), (theirs, false)] {
        let (tasks, jobs): (Vec<JobCompletion>, Vec<JobCompletion>) = batch
            .into_iter()
            .partition(|c| c.kind == BackgroundKind::Subagent);
        if !jobs.is_empty() {
            let mut s = completion_notice(&jobs, mine);
            s.push_str(&waiting_on_these(&jobs, rows));
            out.push(s);
        }
        if !tasks.is_empty() {
            let mut s = subagent_notice(&tasks, mine);
            s.push_str(&waiting_on_these(&tasks, rows));
            out.push(s);
        }
    }
    out
}

/// **The plan rows that wait on these completions, as one line under the notice** — or nothing
/// when no row does.
///
/// The operator, 2026-10-11: *"the completion should mention associated todos"*. A row attached
/// with `wait_on` (a `TodoNeed::Job` or `TodoNeed::Child` edge) is the plan's own statement that
/// this ending is what it was waiting for, and the completion is the moment that row has to move —
/// so the notice names it, on both halves of the board (the model's and the operator's), rather
/// than leaving the model to remember the edge it attached hours ago. Measured on pg-noop the same
/// day: a job's completion arrived, the model read it, and the CRITERION row waiting on it was left
/// `in_progress` until the plan check came round to ask.
///
/// **One line, after the closing paragraph, opening with [`WAITING_ON_THIS`] or
/// [`WAITING_ON_THESE`]** — the head's fold and job card recognise it by those words and nothing
/// else. Only OPEN rows (`pending`, `in_progress`): a row that is done or set aside owes nothing.
/// Row text is cut the way the check cuts a job's description (`said_short`), because this is a
/// pointer to the row and not its restatement.
fn waiting_on_these(done: &[JobCompletion], rows: &[TodoItem]) -> String {
    use letibot_tokencore::store::{TodoNeed, TodoStatus};
    let waits = |row: &TodoItem, c: &JobCompletion| {
        row.needs.iter().any(|n| match n {
            TodoNeed::Job { handle } => {
                c.kind == BackgroundKind::Job
                    && (*handle == c.job || c.slug.as_deref() == Some(handle.as_str()))
            }
            TodoNeed::Child { id } => c.kind == BackgroundKind::Subagent && *id == c.job,
            _ => false,
        })
    };
    let one = done.len() == 1;
    let named: Vec<String> = rows
        .iter()
        .filter(|r| matches!(r.status, TodoStatus::Pending | TodoStatus::InProgress))
        .filter_map(|r| {
            let on: Vec<&JobCompletion> = done.iter().filter(|c| waits(r, c)).collect();
            if on.is_empty() {
                return None;
            }
            let text = letibot_tools::builtins::todo::said_short(&r.content);
            let whose = letibot_tools::builtins::todo::whose_row(&r.by);
            Some(if one {
                format!("\"{text}\" ({whose})")
            } else {
                let handles: Vec<String> = on.iter().map(|c| format!("`{}`", c.job)).collect();
                format!("\"{text}\" ({whose}, on {})", handles.join(", "))
            })
        })
        .collect();
    if named.is_empty() {
        return String::new();
    }
    let verb = match (one, done[0].kind) {
        (true, BackgroundKind::Job) => "it has ended",
        (true, BackgroundKind::Subagent) => "it has finished",
        (false, BackgroundKind::Job) => "they have ended",
        (false, BackgroundKind::Subagent) => "they have finished",
    };
    let them = if named.len() == 1 { "it" } else { "them" };
    format!(
        "\n{}{} — update {them} with `todo_write` now that {verb}.",
        if one {
            WAITING_ON_THIS
        } else {
            WAITING_ON_THESE
        },
        named.join(", ")
    )
}

/// **The endings these rows wait on that the model has not digested yet**, added to `jobs` — the
/// half of `Harness::running_jobs` that reads the watcher set's read record
/// ([`JobWatchers::grace_left`]), free so it is asserted without a session. A job `jobs` already
/// calls running is skipped: running is its own quiet, and the grace is for after.
pub(crate) fn graced_endings(
    mut jobs: letibot_tools::builtins::todo::RunningJobs,
    rows: &[TodoItem],
    w: &JobWatchers,
    cap: std::time::Duration,
    now: std::time::SystemTime,
) -> letibot_tools::builtins::todo::RunningJobs {
    use letibot_tokencore::store::TodoNeed;
    for row in rows {
        for need in &row.needs {
            let handle = match need {
                TodoNeed::Job { handle } if !jobs.is_running(handle) => handle,
                TodoNeed::Child { id } => id,
                _ => continue,
            };
            if let Some(left) = w.grace_left(handle, cap, now) {
                jobs = jobs.digesting(handle.clone(), left);
            }
        }
    }
    jobs
}

/// The opening words of [`waiting_on_these`]' line for one completion — what the head's fold
/// (`tui`'s `folded_notice`) and job card recognise the line by.
pub const WAITING_ON_THIS: &str = "Waiting on this: ";
/// The same for a group of several completions.
pub const WAITING_ON_THESE: &str = "Waiting on these: ";

/// **The held batch, as one note** (R79): the `[meanwhile]` line, then the settlements in
/// [`completion_notices`]' own words — unchanged, so the head's compact job rows read the groups
/// under the heading exactly as they read a wake's.
fn meanwhile_notice(
    batch: Vec<JobCompletion>,
    me: &str,
    rule: crate::delivery::Rule,
    cap: std::time::Duration,
    rows: &[TodoItem],
) -> String {
    let mut parts = vec![crate::delivery::meanwhile_heading(batch.len(), rule, cap)];
    parts.extend(completion_notices(batch, me, rows));
    parts.join("\n\n")
}

/// **R79's delivery, for the two places that hand a result to the model** — the round boundary
/// ([`HubSteering`]) and a turn's start ([`Harness::wake`] and every door that submits a turn).
///
/// The decision is [`crate::delivery::decide`], pure and asserted there; this is the part that
/// reads the session — its watcher set, its board, its record — and turns what the decision said
/// into notices. Built per use from the harness's own fields, so it holds no state of its own:
/// what is held lives on the watcher set, which is what a child's stop hands up.
#[derive(Clone)]
pub(crate) struct Deliveries {
    watch: Arc<JobWatchers>,
    /// The plan the decision reads. `None` in a test with no board.
    todos: Option<Arc<TodoBoard>>,
    log: Arc<crate::delivery::DeliveryLog>,
    cap: std::time::Duration,
    me: String,
}

/// What [`Deliveries::between_turns`] hands a turn's start: the notices said now (immediates and
/// idle arrivals, in `completion_notices`' words) and the `[meanwhile]` batch, apart so a wake can
/// keep the batch last.
pub(crate) struct BetweenTurns {
    pub(crate) now: Vec<String>,
    pub(crate) batch: Option<String>,
}

impl Deliveries {
    pub(crate) fn new(
        watch: Arc<JobWatchers>,
        todos: Option<Arc<TodoBoard>>,
        log: crate::delivery::DeliveryLog,
        cap: std::time::Duration,
        me: &str,
    ) -> Self {
        Deliveries {
            watch,
            todos,
            log: Arc::new(log),
            cap,
            me: me.to_string(),
        }
    }

    /// **What must be said now**: the sort, its immediates recorded and worded. Everything else
    /// is left held.
    pub(crate) fn immediates(&self, skip: impl Fn(&JobCompletion) -> bool) -> Vec<String> {
        let rows = self
            .todos
            .as_ref()
            .map(|t| t.snapshot())
            .unwrap_or_default();
        let now = crate::delivery::sort(&self.watch, &rows, skip);
        if now.is_empty() {
            return Vec::new();
        }
        self.log.immediates(&now, std::time::SystemTime::now());
        let done: Vec<JobCompletion> = now.into_iter().map(|(c, _)| c).collect();
        // **Delivered, and recorded as such** — the fact the plan check's grace reads
        // (`JobWatchers::grace_left`).
        self.watch.read_now(&done);
        completion_notices(done, &self.me, &rows)
    }

    /// The board the notices name rows from — empty in a test with no board.
    fn rows(&self) -> Vec<TodoItem> {
        self.todos
            .as_ref()
            .map(|t| t.snapshot())
            .unwrap_or_default()
    }

    /// **The held batch, taken and recorded**, as one `[meanwhile]` note — or `None` when nothing
    /// is held.
    pub(crate) fn batch(&self, rule: crate::delivery::Rule) -> Option<String> {
        let held = self.watch.take_held();
        if held.is_empty() {
            return None;
        }
        self.log
            .completions(&held, rule, std::time::SystemTime::now());
        self.watch.read_now(&held);
        Some(meanwhile_notice(
            held,
            &self.me,
            rule,
            self.cap,
            &self.rows(),
        ))
    }

    /// **A round boundary inside a running turn**: the immediates, and the batch only when the
    /// longest-held result has waited past the cap.
    pub(crate) fn at_round_boundary(&self) -> Option<String> {
        let mut parts = self.immediates(|_| false);
        if crate::delivery::starved(&self.watch, self.cap, std::time::SystemTime::now())
            && let Some(b) = self.batch(crate::delivery::Rule::Starvation)
        {
            parts.push(b);
        }
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// **A turn the model is taking anyway**: whatever is immediate, what arrived while the
    /// session was idle, and the batch held while the last turn ran (under [`Rule::Batch`] —
    /// it rides this turn).
    ///
    /// [`Rule::Batch`]: crate::delivery::Rule::Batch
    pub(crate) fn at_turn_start(&self) -> Option<String> {
        let b = self.between_turns(|_| false, crate::delivery::Rule::Batch);
        let mut parts = b.now;
        parts.extend(b.batch);
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// **Everything owed to a session between turns** (R79, after the operator's 2026-10-11
    /// ruling: *"jobs were ran for a reason and held for a reason. once turn ends no reason to
    /// hold"*).
    ///
    /// Holding exists only so a RUNNING turn is not interrupted, so between turns nothing stays
    /// held. What is owed splits by one fact, *did it arrive before the session's last turn
    /// ended* ([`JobWatchers::last_turn_ended`]):
    ///
    /// * **arrived while the model was working** — the held batch, under one `[meanwhile]`
    ///   heading worded for `held_rule` (`TurnEnd` from the turn-end wake, `Batch` when another
    ///   turn's start got to it first);
    /// * **arrived while it was idle** — never held in any sense the model should be told about:
    ///   an ordinary completion notice, beside the immediates, recorded as [`Rule::Idle`].
    ///
    /// The immediates are the sort's as before, and `skip` drops what is nobody's news.
    ///
    /// [`Rule::Idle`]: crate::delivery::Rule::Idle
    pub(crate) fn between_turns(
        &self,
        skip: impl Fn(&JobCompletion) -> bool,
        held_rule: crate::delivery::Rule,
    ) -> BetweenTurns {
        let (out, told) = self.between_turns_unread(skip, held_rule);
        self.read(&told);
        out
    }

    /// **[`Self::between_turns`] without marking anything read** (R91.11): the sentences, and
    /// the results they say, for a caller that has not yet put them in front of the model. It
    /// marks them with [`Self::read`] once it has, or hands them back with [`Self::put_back`]
    /// when the turn that was to carry them never opened.
    pub(crate) fn between_turns_unread(
        &self,
        skip: impl Fn(&JobCompletion) -> bool,
        held_rule: crate::delivery::Rule,
    ) -> (BetweenTurns, Vec<JobCompletion>) {
        // **Taken means owed until the turn that says it begins** — marked before the take,
        // because `pending` and `held` are what a reader waits on and the take is what clears
        // them. Without it the activity read quiet between the drain and the clock.
        if self.watch.pending() || self.watch.held_count() > 0 {
            self.watch.took_a_settlement();
        }
        let rows = self.rows();
        let immediate = crate::delivery::sort(&self.watch, &rows, skip);
        let held = self.watch.take_held();
        let ended = self.watch.last_turn_ended();
        let (busy, idle): (Vec<JobCompletion>, Vec<JobCompletion>) = held
            .into_iter()
            .partition(|c| ended.is_some_and(|e| c.arrived <= e));
        let at = std::time::SystemTime::now();
        if !immediate.is_empty() {
            self.log.immediates(&immediate, at);
        }
        if !idle.is_empty() {
            self.log.completions(&idle, crate::delivery::Rule::Idle, at);
        }
        let mut told: Vec<JobCompletion> = immediate.into_iter().map(|(c, _)| c).collect();
        told.extend(idle);
        let mut said = told.clone();
        let now = if told.is_empty() {
            Vec::new()
        } else {
            completion_notices(told, &self.me, &rows)
        };
        said.extend(busy.iter().cloned());
        let batch = (!busy.is_empty()).then(|| {
            self.log.completions(&busy, held_rule, at);
            meanwhile_notice(busy, &self.me, held_rule, self.cap, &rows)
        });
        if said.is_empty() {
            // Nothing was said after all (a gatekeeper's settlement is nobody's news here), so
            // there is no turn owed and nothing for a reader to wait on.
            self.watch.turn_began();
        }
        (BetweenTurns { now, batch }, said)
    }

    /// **These results are in front of the model now** — the read record the plan check's grace
    /// reads, and a subagent's `delivered` in the store.
    pub(crate) fn read(&self, told: &[JobCompletion]) {
        if !told.is_empty() {
            self.watch.read_now(told);
        }
    }

    /// **These results were never told** — the turn that was to carry them did not open — so
    /// they are held again, for the next break to say.
    pub(crate) fn put_back(&self, untold: Vec<JobCompletion>) {
        if !untold.is_empty() {
            self.watch.hold(untold);
        }
    }

    pub(crate) fn log_firings(&self, fired: &[letibot_tools::exec::monitor::Firing]) {
        self.log.firings(fired, std::time::SystemTime::now());
    }
}

/// A duration in the words a settlement reads with: `4.4s`, `1m12s`.
fn human_secs(ms: u64) -> String {
    let secs = ms as f64 / 1000.0;
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

impl SteeringSource for HubSteering {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        // A handed-back message comes out first, with the flags it had when it was
        // taken: it was taken before anything still in the hub's queue, and an
        // interrupt is not something a later message reorders. It is the same
        // `SteeringMessage` coming out again, so `from_operator` and `urgent` are
        // intact — and it is not recorded in the trail here, because `try_next`
        // records a message when it is taken and this one was already recorded the
        // first time it was taken. See [`Self::give_back`].
        if let Some(m) = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        {
            return Some(m);
        }
        // The head first: somebody is waiting on it. Steering-scoped, so a
        // compaction queued behind this turn stays queued for the worker — see
        // [`Hub::try_steering_command`].
        if let Some(cmd) = self.hub.try_steering_command() {
            return match cmd.kind {
                CommandKind::Prompt { text, attachments } => {
                    // The operator, typing into a running turn. This is the case
                    // §2's *"yeah restart"* is about, and it is the one utterance
                    // that can authorise the action it is racing. Marked as the
                    // operator's so consecutive prompts coalesce into one held
                    // message and a take-back drops them.
                    //
                    // **The chips are read HERE and not by the engine**, because a turn's
                    // library has no business opening the operator's files: what it is
                    // handed is parts. A file that vanished between the frame and this
                    // poll is said on the session's log, so nothing about the prompt is
                    // silent — see `crate::attachments`.
                    if let Some(t) = &self.trail {
                        t.say(Speaker::Operator, &text, Some(Instant::now()));
                    }
                    let (extra, refused) = crate::attachments::parts(&attachments);
                    for said in &refused {
                        self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                            code: "attachment_refused".into(),
                            detail: said.clone(),
                            compaction: None,
                        });
                    }
                    Some(SteeringMessage::operator_with(text, extra))
                }
                CommandKind::Interrupt { reason } => {
                    // **THE CHILDREN FIRST, THEN THIS SESSION.** The operator's design in their
                    // own words — *"we talk to parents and they own lifecycle"* — and this is
                    // the one door an interrupt reaches a turn in flight through. Nothing here
                    // waits for the children: a stop that blocked on its subtree would hold the
                    // turn it is stopping open. Each child is told through its own runner, and
                    // each child stops ITS children when the stop reaches it, which is how one
                    // interrupt walks a whole tree without anybody holding a grandchild's
                    // handle.
                    stop_children_first(self.subagents.as_ref(), &self.hub);
                    Some(SteeringMessage::urgent(reason))
                }
                CommandKind::Message { from, text } => {
                    // **A parent's live correction** — an agent's utterance in a session it is not
                    // seated in, so it is recorded as `Speaker::Agent` and NOT as the operator.
                    // That is the whole of the difference between this and the `Prompt` arm
                    // above, and it is why the variant exists rather than reusing one: an
                    // operator's words can authorise the act they race, coalesce with other
                    // operator text, and be taken back — a parent's cannot.
                    //
                    // `SteeringMessage::normal` is the same door this session's own boundary
                    // already uses for injected context, so a correction arrives as context at
                    // the next round boundary rather than as an interruption: a child told to
                    // change course should finish the call it is in and change, not abandon it.
                    //
                    // **And the words are [`message_from_a_session`]'s**, which is the same text
                    // the between-turns door hands the session (`serve_child`'s
                    // `ChildCommand::Hear`): a session that could tell which door the words came
                    // through would be reading the machinery instead of the message.
                    //
                    // # Which door writes the ROW, and why the row is the injection
                    //
                    // MEASURED 2026-10-29 (`message_between_turns`'s
                    // `a_message_to_a_running_child_lands_in_that_turn`, extended with a
                    // `transcript_item` read): this arm writes NO row, and the child's
                    // transcript gets one anyway — because the row and the injection are the
                    // same thing. The steering message below is handed to the engine, the
                    // engine's `Pending::take_items` turns it into a `User` item with
                    // `Speaker::Agent`, and `Session::append_items` appends that item through
                    // the same writer every other row goes through (`reconcile` + `persist` in
                    // `Harness::run_rounds`). The model reads that row; a head draws that row;
                    // the store holds that row. A second row written here would be a COPY of
                    // the injection, and the child would read its parent's sentence twice.
                    //
                    // What that leaves is one window, and it is the reason
                    // `SteeringMessage::from_a_session` exists: the item is appended at a
                    // **step boundary**, and a round that fails after taking this message
                    // hands it back (`SteeringSource::give_back`) without a boundary ever
                    // arriving. `Harness::run_rounds` closes that window — a message still
                    // carried when the turn is over is written into the transcript there, in
                    // these same words, so the row exists whether or not a turn ever read it.
                    let said = message_from_a_session(&from, &text, self.parent.as_deref());
                    if let Some(t) = &self.trail {
                        t.say(Speaker::Agent, &said, Some(Instant::now()));
                    }
                    Some(SteeringMessage::from_a_session(said, from))
                }
                // **The operator answered a row's question while the turn runs** — R79: an
                // operator's words are immediate, so the answer is applied to the board HERE and
                // the model reads it at this step boundary. Recorded as the operator's, and sent
                // as a notice that stands alone (`normal`): the line is the harness's sentence
                // ABOUT their answer, and a take-back of queued prompts must not drop an answer
                // the board has already taken. A refusal (the row moved on, or an answer the row
                // never offered) is said to the heads and the poll goes on to the next source.
                CommandKind::AnswerTodo {
                    content,
                    question,
                    answer,
                } => {
                    let settled = match &self.todos {
                        Some(board) => {
                            crate::operator_ask::settle(board, &content, &question, &answer)
                        }
                        None => Err("this session has no board".into()),
                    };
                    match settled {
                        Ok(line) => {
                            if let Some(t) = &self.trail {
                                t.say(Speaker::Operator, &line, Some(Instant::now()));
                            }
                            Some(SteeringMessage::normal(line))
                        }
                        Err(why) => {
                            self.hub.publish(SessionEvent::Warning {
                                code: "todo_answer_refused".into(),
                                detail: format!(
                                    "your answer on `{}` was not taken: {why}",
                                    content.trim()
                                ),
                                compaction: None,
                            });
                            self.try_next()
                        }
                    }
                }
                // Nothing else here: the filter above only hands over prompts, interrupts, a
                // parent's message and an answer to a row's question, and anything else stays
                // queued for the worker.
                _ => None,
            };
        }
        // Then anything this harness queued for the boundary.
        if let Some(text) = self
            .injected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        {
            if let Some(t) = &self.trail {
                t.say(Speaker::Agent, &text, Some(Instant::now()));
            }
            return Some(SteeringMessage::normal(text));
        }
        // **Then the room, at the boundary a running turn actually reaches** — firecode's first
        // rule, *listen before you work*.
        //
        // A room event becomes a sentence on the connection's own thread (`crate::room::wake`) and
        // `Harness::wake` reads that queue between turns. That is one door, and it is the wrong one
        // for a session in the middle of a long turn: the message that says *`s-b` claimed the tag
        // you were about to take* arrives after the work it would have changed. MEASURED 2026-10-31
        // (branch `agent/the-bell-per-client`): `try_next` had five sources and none of them was the
        // room, so a room note waited for the turn to end while the engine was polling this function
        // every step boundary.
        //
        // **It is the same queue, taken by whichever door gets there first** — `wake::take` empties
        // it — so a note is delivered exactly once, mid-turn or between turns, and no bookkeeping is
        // needed to keep the two from both speaking.
        let room = crate::room::wake::take(&self.hub.session_id());
        if !room.is_empty() {
            let text = room.join("\n\n");
            if let Some(t) = &self.trail {
                t.say(Speaker::Agent, &text, Some(Instant::now()));
            }
            return Some(SteeringMessage::normal(text));
        }
        // Then a background job or subagent of THIS session's own that has ended. It is
        // here, above the monitors, because the monitors block below returns early when
        // this session has no monitor registry — and a read-only seat that can only
        // `task` is exactly the session that has completions and no monitors.
        //
        // **R79: only what the work waits on comes in mid-turn**, for a root and a child
        // alike — a row's `when`/`needs`, a wait, `wake: "now"`, or the failure of one of
        // those. Everything else is held for the next turn boundary, unless it has been held
        // past the cap. See `crate::delivery`. (Before R79 a root heard nothing here and
        // everything as a wake turn after; a child heard everything here.)
        if let Some(d) = &self.deliveries
            && let Some(text) = d.at_round_boundary()
        {
            if let Some(t) = &self.trail {
                t.say(Speaker::Agent, &text, Some(Instant::now()));
            }
            return Some(SteeringMessage::normal(text));
        }
        // Then a monitor that fired while this turn was running. Between turns the
        // same firing arrives through `Harness::wake`; the shared cursor is what
        // stops it arriving twice.
        let monitors = self.monitors.as_ref()?;
        let since = self.monitor_cursor.load(Ordering::SeqCst);
        let settled = monitors.settled_count();
        if settled <= since {
            return None;
        }
        let fired: Vec<_> = monitors.firings().into_iter().skip(since).collect();
        self.monitor_cursor.store(settled, Ordering::SeqCst);
        if fired.is_empty() {
            return None;
        }
        // A firing is immediate by R79's rules and recorded as one — every one, even those
        // the sentence leaves out (R93.C8).
        if let Some(d) = &self.deliveries {
            d.log_firings(&fired);
        }
        let text = monitor_notice(&fired)?;
        if let Some(t) = &self.trail {
            t.say(Speaker::Agent, &text, Some(Instant::now()));
        }
        Some(SteeringMessage::normal(text))
    }

    /// The take-back the operator issued from a head: the hub drops the head's
    /// still-queued prompts, and the engine's `Pending` drops the held operator
    /// text when this returns `true`. The two halves of one recall — a prompt
    /// typed behind a long tool call is still in the hub's queue, one absorbed
    /// during generation is already held here, and the operator's Up pulled the
    /// whole thing back into the composer.
    fn try_withdraw(&mut self) -> bool {
        self.hub.try_withdraw_command()
    }

    /// **Hand a message back to the source, for a turn that failed.**
    ///
    /// The mirror of the take in [`Self::try_next`]: a message is out of the hub's
    /// queue the moment `try_next` dequeues it, and a turn that fails commits
    /// nothing (§5.7), so the words go back rather than being appended — appending
    /// them would advance the ledger and the prefix on an error path, the same loss
    /// with a different sign. They are held in `self.held` and drained by the next
    /// [`Self::try_next`] before the hub's queue, so the retried or next round's
    /// greedy pre-poll puts them in the prompt.
    ///
    /// **No `t.say` on the way back.** `try_next` is where a message is recorded in
    /// the trail as `Speaker::Operator` when it is taken; recording it here would
    /// record one that may never be consumed. The message keeps its
    /// `from_operator`/`urgent` flags because it is the same `SteeringMessage`
    /// coming out again.
    fn give_back(&mut self, msgs: Vec<SteeringMessage>) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(msgs);
    }
}

/// **The daemon's own server, exactly as this session's daemon was started against
/// it** — what `/models local` returns to.
///
/// Captured at open, before any switch can move anything, because a switch
/// overwrites the very fields this holds: a session that moved to another fleet
/// model's weights has an `engine` rendering another dialect and a `cfg.endpoint`
/// naming another box, and "back to local" is a RETURN, not a no-op. Before this
/// existed the return was partial in a way nobody could see: the window came back
/// (its own remember-on-first-move) while the address stayed wherever the last
/// fleet switch put it, and the line still said *"turns go to the local server"*.
///
/// The vocabulary and the wiring are held here as `Arc`s — the same ones
/// [`Parts`] opened with — so the return does not reload a tokenizer or rebuild a
/// renderer; it reseats the pair the daemon started with.
#[derive(Clone)]
struct OwnServer {
    endpoint: Endpoint,
    model: String,
    /// The GGUF the daemon started on, or `None` when it started on the byte
    /// vocabulary — the same `Option` `Config::vocab_gguf` carries since main
    /// made the vocabulary optional. `vocab` below holds the thing itself
    /// either way, and its `source()` is the honest name for both.
    vocab_gguf: Option<PathBuf>,
    dialect: crate::dialect::Dialect,
    media_marker: Option<String>,
    sampling: serde_json::Value,
    vocab: Arc<Vocab>,
    wiring: Arc<Wiring>,
}

/// One session: the engine, the transcript, the tools, the log and the store.
pub struct Harness {
    cfg: Config,
    /// **The right to append to this session's transcript**, claimed at open and given back when
    /// this harness is dropped ([`crate::writer`], `letibot_tokencore::store::writers`).
    ///
    /// A field rather than a call at every write, because the claim is a LIFETIME and not a
    /// permission check: it is taken once, it is what a second process's open is refused by, and
    /// the value's only job is to be dropped at the right moment. `None` for a session with no
    /// store, and for one whose live copy is on another endpoint.
    _writer: Option<crate::writer::Claim>,
    /// **The daemon's mode rows**, which a `/mode` taken by a running turn writes
    /// (`apply_queued_mode`, through [`crate::modes::record`]).
    mode_store: std::sync::Arc<std::sync::RwLock<crate::modes::ModeStore>>,
    /// **The worktree and branch this session's work must end up on**, when it is a
    /// `task_start` child with a fresh worktree; `None` for every other session. Read as a turn
    /// is about to end ([`deliverable::EndOfWork`]): a dirty tree is told once, in the harness's
    /// voice, and the turn goes on. Set by the spawn (`run_to_completion`), not by `open`,
    /// because the placement is the runner's fact and the config does not carry it.
    deliverable: Option<deliverable::Deliverable>,
    /// **When the prompt this turn is serving arrived, carried across its rounds.**
    ///
    /// The operator, watching the composer: *"it should be still responding even while you do
    /// tools calls and such, and not reset, currently it resets."* That is the arithmetic, and
    /// the cause is structural: `run_turn_steered` is called INSIDE the round loop, so the
    /// `TurnStarted` the head times from fires once PER ROUND — the clock restarted at every
    /// round and the row read `2.1s` a minute into a turn.
    ///
    /// A turn is one prompt, however many rounds it takes, so **the turn's start belongs to the
    /// prompt and not to a round.** Stamped where the prompt arrives and published on every
    /// round's `TurnStarted`, which is the only place that can see both.
    ///
    /// The monotonic ms the head should count from, or `None` to keep counting from its own
    /// (a snapshot turn, where neither end measured anything).
    turn_began_ms: Option<u64>,
    /// **Whether a PROMPT is in flight** — the span [`Harness::begin_turn_clock`] and
    /// [`Harness::end_turn_clock`] bracket, from the prompt being taken to its loop returning
    /// (final answer, interrupt, failure, wall-continuation, or a stop on a card).
    ///
    /// **The activity's `Turn` and this are the same span, deliberately** (`PROTOCOL_VERSION` 52):
    /// the engine publishes `TurnStarted`/`TurnFinished` once per ROUND, and a round that ended
    /// with tool calls is always followed by another round of the same prompt — so a `Turn` derived
    /// from those events would drop to *quiet* at every round boundary, which is the false wake
    /// the whole mechanism exists to remove (measured: the bridge's `wait` exited there, QC's
    /// `wait` exited there, and so did R55's flicker on this head's own row).
    ///
    /// An `Arc<AtomicBool>` because [`crate::activity`]'s source closure reads it from whatever
    /// thread publishes, and the harness holds it `&mut` while a prompt runs. Set and cleared in
    /// the two clock functions and nowhere else: the clock's span IS this span, so a second writer
    /// would be a second answer to one question.
    prompt_in_flight: Arc<AtomicBool>,
    /// **Whether the shared tail is running a turn on this harness right now** — the gate on
    /// [`Harness::begin_turn_clock`], and the reason it is not a clock in its own right.
    ///
    /// A turn's clock belongs to a TURN, and a turn is [`crate::turn_tail::run_turn`]'s: it is the
    /// one place that knows a turn will be ended ([`Harness::end_turn_clock`]) and that a door may
    /// end up saying nothing at all. The prompt is not the only caller of `run_rounds` —
    /// `Harness::system_update` (a compaction's fabric block, `/compact`, the `m1` script) appends
    /// an item and runs its rounds outside any turn, and a clock started there would never be
    /// cleared: the session would read `waiting on: turn` for the life of the daemon.
    turn_open: bool,
    /// The server thread answers the head's `Settings` from the registry, so
    /// what is running is what the pane shows.
    session_registry: Arc<letibot_sessionlog::registry::Registry>,
    /// **The daemon's subagent journal, which is how a plan knows a child is still running.**
    ///
    /// A row may wait on a CHILD ([`letibot_tokencore::store::TodoNeed::Child`]), and whether that
    /// child is running is not a fact any board holds: the journal is where every spawn and every
    /// finish lands (`opening`/`running`/`done`/`failed`), so the idle check reads it here beside
    /// the session registry — which supplies the other half, when the child was opened. See
    /// `crate::sessions::child_sessions`, the one reader.
    ///
    /// It rides the harness rather than a config because it is the DAEMON's record and not this
    /// session's, and it is shared: the same `Arc` the task runner spawns through, so a child
    /// this session started and one a peer started are one fact.
    tasks: Arc<crate::tasks::TaskJournal>,
    /// Where the mode came from, for the settings row: the project store or
    /// the daemon's flag/default. Decided at open and not kept by `Config`.
    mode_source: String,
    engine: TurnEngine,
    /// **The counter this session's standing notes are measured with** — the section's own,
    /// and a field rather than a read of `self.engine` because the notes pane's read happens
    /// on the SERVER's thread while the engine lives on this one. It tracks the engine: set at
    /// open from `Parts`, and reassigned wherever `self.engine` is, so a session that moved
    /// weights measures its notes with the vocabulary it is now speaking — which is the same
    /// counter `reseat_target` assembles the next section with.
    ///
    /// See [`Harness::install_notes_source`], the only reader.
    note_vocab: Arc<Vocab>,
    /// **The skills this daemon loaded**, which the prompt's index is composed from and the page's
    /// rows are read off — the same `Arc` the `skill` tool and the dashboard hold, so a pane and a
    /// prompt cannot list two different corpora.
    skills: Arc<letibot_tools::builtins::skill::SkillRegistry>,
    session: Session,
    /// **Shared, because the operator's run does not run on this harness's thread.**
    ///
    /// A `!` line is handed to a thread of its own (see `crate::bangrun`) and that thread
    /// invokes through THIS runtime — one exec host per session is a fact (`job_list`, a
    /// promotion, the run ender at shutdown all name jobs in it), so a second runtime would
    /// be a second list of the same jobs. Everything the runtime holds is either read-only
    /// after open or synchronised; the gate, the one field that mutates in place, sits
    /// behind its own mutex for exactly this reason — see `ToolRuntime::gate`.
    runtime: std::sync::Arc<ToolRuntime>,
    hub: Arc<Hub>,
    store: Option<Store>,
    /// **The corpus sink, kept beside the gate's own copy** — R24 part two, decision 2.
    ///
    /// The gate writes every row it decides; an operator's OWN call is not a decision the
    /// gate took, so nothing in the gate would write it. It must still land in the same
    /// corpus, through the same sink, or the calibration reads a call the person ran as an
    /// auto-admit — which is the defect decision 2 exists for. One `Arc`, cloned before the
    /// gate takes its own.
    corpus: Option<std::sync::Arc<dyn letibot_tools::CorpusSink>>,
    transcript_id: String,
    /// The stable prefix **this transcript was built with** — which on a resume is
    /// the session's own, not the one this daemon would render now. Compaction
    /// forks with it, for the same reason a resume keeps it: rewriting message 0
    /// is what forces a full cold re-prefill.
    prefix: StablePrefix,
    /// The store's content address for [`Harness::prefix`]. Empty when there is no
    /// store, and never read in that case — compaction is refused before the id
    /// would be used.
    prefix_id: String,
    /// How many ledger rows have reached the store.
    persisted: usize,
    system_updates: u64,
    /// The turn this harness last started. §4.5: a failure has to name one.
    last_turn_id: String,
    /// Read at open time from the backend, the gate and the seated schemas, so the
    /// adjudication disclosure is a reading rather than a claim. See [`GateWiring`].
    wiring: GateWiring,
    /// True only while [`Harness::compact`] is running its summary turn. See the
    /// context-wall check in `run_rounds` for why that turn must be exempt.
    compacting: bool,
    /// **Where the provider last counted this conversation** (R72) — the anchor the
    /// wall's resident count is rolled forward from. `cfg.ledger_scale` is kept as
    /// the pair `(ledger now, resident now)` that [`Harness::measure_resident`] last
    /// computed from it, so every `Config` method that converts reads the current
    /// count and not a ratio from another moment.
    ///
    /// `None` with `cfg.ledger_scale` set is a scale this session did not measure —
    /// a subagent's, inherited from its parent — and keeps the old ratio reading.
    /// See [`WallAnchor`].
    wall_anchor: Option<WallAnchor>,
    /// How this daemon RENDERS a prompt: the dialect's template and the tool-schema
    /// serialiser. Kept so a re-seat can build a new stable prefix the same way
    /// `open` built the first one — one renderer, so the prefix a re-seat writes
    /// and the prefix a fresh session writes cannot drift.
    render: std::sync::Arc<Wiring>,
    /// The prerequisites this session's wiring supplies and the classes it seats,
    /// read once at open. What `/mode` checks a new point against, so a move that
    /// the open would have refused is refused the same way rather than taken and
    /// then failing on its first call.
    supplies: Option<(Vec<letibot_tools::mode::Prereq>, letibot_tools::mode::Seats)>,
    /// `Some` when this harness was rebuilt from the store rather than opened fresh.
    /// The daemon prints it; a head is told through the log, by the rows themselves.
    resumed: Option<ResumeReport>,
    /// What `open` decided that the operator would otherwise learn a turn later,
    /// for a session that was NOT resumed — the resume report carries the same
    /// notes for one that was. Measured 2026-09-14: a one-shot started with
    /// `--mode allow-all` ran at `writes allowed` because the project store said
    /// so, and the note saying so went into a report nobody printed, because the
    /// session was new.
    open_notes: Vec<String>,
    /// A title this harness derived and the daemon has not yet published. See
    /// [`Harness::take_new_title`].
    new_title: Option<String>,
    /// **The `!` sequence**: how many of the operator's own shell lines this session
    /// has run. The `call_id` of each is `bang-<n>`, minted here because — unlike the
    /// door — no head chooses one: the head sends the line and the daemon owns the run.
    bang_seq: u64,

    /// **The worker's serialization state for this session's operator runs** — whether
    /// one is in flight on its own thread, and the lines parked behind it. Attached by
    /// `Sessions` at open ([`Self::set_bang_state`]) as a clone of the same `Arc` the
    /// dispatch arm reads, so the round boundary's pickup (`apply_queued_head_run`)
    /// and the worker's settle arm answer ONE question — *is a run going?* — from one
    /// place. `None` only in a harness no `Sessions` built (the tests that drive one
    /// directly), where the pickup keeps its old synchronous timing.
    bang: Option<std::sync::Arc<crate::bangrun::State>>,

    /// Who said what, so the gate's adjudicator can see what authorised an action.
    /// Fed at every append, because the provenance is only knowable there.
    trail: Arc<TrailMirror>,
    /// **What this session's harness HOLDS, restated at every append** — R66's other half, see
    /// [`crate::footprint`]. The hub's census reads these through the source registered at open,
    /// and they are atomics because the reader is the memory watchdog's thread and the writer is
    /// this one: a lock here would be a lock a turn holds, taken from the census.
    footprint_gauges: Arc<crate::footprint::Gauges>,
    /// **The last question this session asked a person, and the answer it got** — filled by
    /// the session's own [`crate::answers::HeadQuestioner`] and read back by the round loop,
    /// which is the one place a tool result is appended. See [`Harness::settle_asked`] for
    /// why the person's half of the exchange is written down there and not by the tool.
    asked: crate::answers::AskLog,
    /// **The calls this session's gate admitted without asking anybody, waiting to be said**
    /// (D16).
    ///
    /// The gate runs inside the runtime, off this struct, so it hands the sentence over rather
    /// than speaking it — [`Harness::flush_gate_admissions`] writes one transcript row per
    /// notice at a round boundary, which is the only place a row can go that does not sit
    /// between a tool call and its result.
    gate_admissions: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// The session's todo list, shared with the tool that writes it. The harness
    /// is the persister: [`Harness::flush_todos`] compares the board's version
    /// with the last one it handled and does the store write and the
    /// announcement when they differ.
    todos: Arc<TodoBoard>,
    /// The board version this harness last persisted and announced.
    todos_version: u64,
    /// **The questions the heads were last told this board waits on the operator for** —
    /// `(row, question)`, compared at every flush so each is announced once when asked and once
    /// when settled. Seeded from the board at open: a restart does not re-announce, the settings
    /// row carries the state to a head that attaches.
    asks_said: Vec<(String, String)>,
    /// **The row the last plan-check named**, so the check keeps asking about THAT row rather than
    /// whatever happens to head the queue now. See `unfinished_plan_for`: the choice is sticky, and
    /// this is the one field that remembers it.
    nag_choice: Option<String>,
    /// **The stale-prompt sentence the model has already been told**, so it is told ONCE.
    ///
    /// The operator, on the reseat card: *"regarding reseat - since it is important id suggest
    /// to show me "select" with one option - reseat and abstain and then when on abstain keep
    /// reseat select open. this way i keep reseat in mind because it stares at me yet can
    /// continue at my risk"*. The decision is the person's and it stares at them from their
    /// head, so the model is not the one to keep it in mind — the plan-check used to append the
    /// sentence to every check and tell the model to keep asking, which the operator had called
    /// nagging. Compared by text, so a DIFFERENT stale sentence (the build moved again) is told
    /// once more; cleared by the fork that re-seats. Held in memory: a daemon restart may tell
    /// the model once more, which is one line per restart and not one per nag window.
    stale_told: Option<String>,
    /// **A fork whose rows are not all stored yet** (R91.2) — sealed by [`Harness::seal_fork`]
    /// once they are.
    unsealed_fork: Option<UnsealedFork>,
    /// **This turn's metrics, as it runs** (TODO R81) — begun and closed by the turn tail, fed
    /// by the round loop. See `crate::instruments`.
    pub(crate) meter: crate::instruments::TurnMeter,
    /// T21.3's encoder and error signal. Always constructed and always attached —
    /// measuring costs nothing and `Verification::NoEncoder` is a state whose
    /// reason is ours rather than the model's.
    intent: Arc<IntentLedger>,
    /// What the next step boundary will inject: intent findings, monitor firings.
    /// Shared with the session's [`HubSteering`].
    injected: Arc<Mutex<VecDeque<String>>>,
    /// **What a round handed back to its steering** — shared with every [`HubSteering`]
    /// this harness builds, so a message taken out of the hub and handed back by a
    /// failed or interrupted round outlives the round (R91.1). See [`Harness::run_rounds`]
    /// for what happens to it when the turn ends.
    handed_back: Arc<Mutex<VecDeque<SteeringMessage>>>,
    /// This session's monitors, when the backend can start processes.
    monitors: Option<Arc<Monitors>>,
    /// **This session's connection to the box's room** (the room's slice 2), when there is a
    /// coordinator and this session may speak in it — a top-level session, and no other.
    ///
    /// It is on the harness because two of its three duties are the harness's own moments: the beat
    /// starts and stops with [`Harness::begin_turn_clock`]/[`Harness::end_turn_clock`], and the
    /// tool behind it is registered in this session's registry. The third — the bell — is the
    /// connection's own thread pushing a sentence into [`crate::room::wake`] and ringing the
    /// worker's bell; this field is what keeps the connection alive for the session's life.
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
    /// **The session's children, as the thing that owns their lifecycle.**
    ///
    /// The same `Arc` the tool registry holds for `task`/`task_result`/`task_message` and the
    /// same one `job_watch` walks to their slots — one object, three readers, because a second
    /// runner would hand out handles its sibling could not collect or stop.
    ///
    /// The harness keeps it because **stopping is the session's business and not a tool's**: an
    /// interrupt arrives as a `CommandKind`, and *stop my children, then me* has to happen on
    /// this session's own thread — mid-turn through [`HubSteering`]'s interrupt arm, between
    /// turns through [`serve_child`]'s stop arm, and after the fact (nothing running) through
    /// [`crate::sessions::Sessions::dispatch`]'s. Before this field the runner was reachable
    /// only from the tool registry and the watcher set, so an interrupt had nothing to reach
    /// its children with — which is the defect, measured, that this exists for.
    subagents: Arc<dyn letibot_tools::builtins::task::TaskRunner>,
    /// **The merge-queue reviews this session is hosting** — who is reviewing what, and what
    /// they have handed in. See [`Harness::serve_reviews`], and
    /// [`letibot_tools::builtins::verdict::ReviewDesk`] for why it is a shared `Arc` rather than a
    /// list on this harness.
    ///
    /// **The child's verdict arrives HERE, and not in its prose.** The reviewer calls
    /// `review_verdict`, the desk records what it handed in, and the next pass of this session
    /// takes it and writes the entry's row. A child that answers in prose without ever calling it
    /// has judged nothing ([`review_outcome`] counts that attempt rather than inventing one), and
    /// a child that dies without calling it leaves the row where it is for the queue to ask
    /// again.
    reviews: Arc<letibot_tools::builtins::verdict::ReviewDesk>,
    /// **Every gatekeeper child this session has started**, answered or not: a settlement naming
    /// one is the review's, not news for this session's model, and is dropped from the notices
    /// (`Harness::wake`).
    ///
    /// **The reason changed with the ownership, and it is still the reason.** While the review ran
    /// under the tree the branch came from, this session's model had nothing to do with a review at
    /// all. Now the review is a `task` child of the QUEUE's session (`Harness::serve_reviews`), so
    /// the settlement does arrive at the session that owns the row — and it is still not news,
    /// because `Harness::wake` calls `serve_reviews` FIRST: by the time the notices are built, the
    /// verdict the child handed in has been written onto the entry's review row and the row is off
    /// this session's board. What the queue's session hears about its rows is the BOARD (a row
    /// `in progress` with the child's handle, or gone), not a sentence about a child that has
    /// already been collected.
    gatekeepers: std::collections::HashSet<String>,
    /// **What this session's model is owed about the children a restart brought back** — one
    /// `[restart]` line per child that was resumed or could not be (`crate::subagent_record`).
    /// Filled by [`Harness::restore_after_restart`] when the session is opened, and said by the next
    /// turn the session takes ([`Harness::wake`], or a restarted child's own continuation).
    restart_notices: Vec<String>,
    /// Whether this harness has said its workspace is gone (R92.24) — once, at the first wake
    /// that found it so.
    workspace_gone_said: bool,
    /// The monitor names live at the last [`Harness::sync_monitor_records`], sorted.
    monitors_seen_live: Vec<String>,
    /// **The queue entries this session's model has been told about** — that its repository needs a
    /// merge gate, or that no gatekeeper can judge its branch — so each is said once per daemon.
    /// See [`Harness::queue_notices`] and [`Harness::handover_notices`].
    ///
    /// One set for both, because an entry has one `evidence` string and is therefore in one of the
    /// two states at a time: the second telling would be about the same row, and a session told
    /// twice about one entry is the nag the set exists to prevent.
    gate_told: std::collections::HashSet<String>,
    /// **The outage the retry ladder is in, if any** — one per endpoint, told ONCE when it ends.
    /// See [`Outage`] for why the row is written at the end and not per attempt.
    outage: Option<Outage>,
    /// The cloud provider the turns go to, when the session has one. `None` is
    /// the local server through the engine's own `/completion` path — **unless**
    /// [`Harness::key_wanted`] is set, which is a provider with no key yet.
    provider: Option<Box<dyn letibot_backend::MessagesBackend>>,
    /// **A provider chosen and not yet usable, because no key for it resolves.**
    ///
    /// It used to be a refusal at open: the daemon bound its socket and died before a
    /// head could attach, so the person who could fix it never saw the sentence. Now the
    /// session opens and the first turn asks for the key on the masked secret card —
    /// the sudo password's machinery, so the key never enters an event — saves it to
    /// providers.toml (0600), and builds the provider. See [`Harness::obtain_key`].
    key_wanted: Option<crate::config::ProviderConfig>,
    /// **The local server's own context window**, kept from before the first
    /// switch to a provider so `/models local` can put it back.
    ///
    /// `Option<Option<u64>>`: the outer says whether this session has ever left
    /// the local server, the inner is the window itself — which is legitimately
    /// `None` when `/props` reported nothing. Collapsing the two would make
    /// "never switched" and "switched away from a server with no window" the same
    /// state, and only one of them should restore anything.
    local_window: Option<Option<u64>>,
    /// **What `/models local` returns to**: the daemon's own server, vocabulary
    /// and dialect, captured at open before any switch moved them. See
    /// [`OwnServer`] for why a return needs a record rather than a re-derivation,
    /// and [`Harness::set_own_server`] for the return itself.
    own_server: OwnServer,
    /// **The same fact, shared with the subagent runner** — so a child spawned with
    /// `model: "local"` plans its compaction against the daemon's own server window
    /// rather than the cloud model's number the parent is carrying. Written wherever
    /// [`Harness::local_window`] is written; see [`HarnessTaskRunner::local_window`].
    local_window_cell: Arc<std::sync::Mutex<Option<Option<u64>>>>,
    /// **This session's point, as it stands now**, shared with the subagent runner.
    ///
    /// A cell rather than `cfg.mode` because the runner holds a *clone* of this config,
    /// taken at open ([`HarnessTaskRunner::base`]), and `/mode` moves the point without
    /// touching that clone — so a child spawned after a move came up at the point its
    /// parent had opened at. Written wherever the point moves
    /// ([`Harness::set_mode_consented`], and once at open); read at every spawn. See
    /// [`HarnessTaskRunner::point`].
    point_cell: Arc<std::sync::Mutex<letibot_tools::mode::Mode>>,
    /// **The model this session switched away from, kept until a round proves the new
    /// one works** — so a switch that fails says how to go back.
    ///
    /// The failure it exists for is the one an operator cannot see the shape of:
    /// MEASURED 2026-10-25, a long session was switched to `glm-coding`, the provider
    /// refused its very first request (`messages.content.type is invalid`), and every turn
    /// after that was refused the same way. The operator's only move was to remember what
    /// the session had been on and type it back — and the daemon knew both the name and
    /// the command. See [`Harness::switch_back_notice`].
    switch_back: Option<SwitchBack>,
    /// **A model switch that arrived while a prompt was in flight**, waiting for that prompt
    /// to end — see [`Harness::set_provider`] and [`Harness::apply_pending_switch`].
    ///
    /// It is a field and not a queue of the hub because it is not a command anybody replays: the
    /// request is answered the moment it is taken (`"turns go to X when this turn ends"`), and
    /// what is left to do is one act, at one moment the harness already has
    /// ([`Harness::end_turn_clock`]). A queue would let two switches stack and make *which one
    /// wins* a question about the order a worker happened to drain them in.
    pending_switch: Option<PendingSwitch>,
    /// **The turn the daemon owes after it moved this session off a model that was out** — the
    /// notice that opens it, built where the refusal is ([`Harness::fall_back_from`]), taken by
    /// [`crate::turn_tail::run_turn`] where a turn ends.
    ///
    /// The operator's ruling, 2026-10-12, is the whole requirement: *"when it finally applied the
    /// turn didnt continue"*. Ending the turn on the refusal and moving the session was half an
    /// answer; the work is carried on by a NEW turn, opened by a user item in the harness's own
    /// voice — which is also what a model's first request has to follow (see
    /// [`Harness::set_provider`]).
    ///
    /// **At most one**: the continuation turn is `Door::Fallback`, and a turn that is itself a
    /// continuation never earns another (see the tail's own note), so a fallback chain cannot
    /// ping-pong between two names — a chain that pays a model change per turn and never stops.
    fallback_continuation: Option<String>,
    /// **This turn IS the automatic continuation a fallback earned** — set while it runs, so
    /// [`Harness::fall_back_from`] moves the session to the next name without arming a second
    /// continuation. See `fallback_continuation` for why the bound exists.
    continuation_of_fallback: bool,
    /// **The session's plugin host, once the daemon has one** — the same live `Arc` the `plugin`
    /// tool asks through (`crate::plugins::desk::SessionPlugins`), handed over here because the
    /// two halves are built at different moments: the tool is seated when the harness opens, and
    /// the host when the DAEMON opens the session (`Sessions::open_plugins`, which is the one
    /// place a hub and a pane driver exist).
    ///
    /// `None` in a session the daemon did not open — a subagent, whose harness its parent's runner
    /// builds — and `plugin scaffold`/`build` work there, because they are file and cargo
    /// operations on the session's own plugin directory.
    plugin_slot: Arc<std::sync::Mutex<Option<Arc<crate::plugins::PluginHost>>>>,
    /// **The mode confirmation a head asked for and nobody has answered**
    /// (`PROTOCOL_VERSION` 54, B8). One per session: a second ask supersedes it.
    mode_confirm: Option<PendingModeConfirm>,
}

/// **A confirmation of `allow-all` that is waiting for a person** — the daemon's half of the
/// card raised by another head's ask.
///
/// The `asked` list is the rule and not bookkeeping: the card went to those heads, and only
/// those heads can answer it. The bridge that asked for it is never among them, which is what
/// makes *the bridge never consents for itself* structural rather than a promise.
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

/// The sentence a superseded request leaves behind — its asker's wait ends with the daemon's
/// own words rather than with the new requester's.
fn superseded(name: &str) -> String {
    format!(
        "the `{name}` confirmation was superseded by a newer request before anybody answered \
         it, so nothing changed; the newer one is on the screen now"
    )
}

/// **A switch waiting for the turn in flight to end** — recorded, said, and applied where the
/// prompt span closes.
///
/// MEASURED 2026-10-26, the operator's session: `/models deepseek/deepseek-flash` typed while a
/// round on `glm-coding/glm-5.3` was failing was applied INSIDE the retry loop, so the round was
/// re-issued on deepseek carrying the rest of GLM's turn — a run of assistant tool-call rounds
/// with no `reasoning_content`, which deepseek's thinking mode refuses (`http 400: The
/// `reasoning_content` in the thinking mode must be passed back to the API`). The operator's
/// sentence for the rule: *"we can switch after turn ends"*.
struct PendingSwitch {
    /// The argument [`Harness::set_provider`] will be given when the turn ends: `Some` is a
    /// metered provider, `None` the daemon's own server.
    choice: Option<crate::config::ProviderConfig>,
    /// The model it lands on, in `/models`' spelling (`deepseek/deepseek-flash`, `local`), read
    /// when it was recorded — so the sentence that says what will happen does not have to build
    /// the provider to name it.
    model: String,
}

/// **The model a `set_provider` argument would land on**, in [`Harness::model_name`]'s spelling
/// (`deepseek/deepseek-flash`, `local`) — so a switch that is still pending can be named, and
/// compared with what answers now, without building a provider for it.
fn model_name_of(choice: &Option<crate::config::ProviderConfig>) -> String {
    match choice {
        None => "local".to_string(),
        Some(pc) => match &pc.model {
            Some(m) => format!("{}/{m}", pc.name),
            None => pc.name.clone(),
        },
    }
}

/// **The sentence a failed switch earns** — apart from [`Harness::switch_back_notice`] so
/// the wording and the command are testable without a session, a store and a provider.
///
/// The command is spelled the way the head's `/models` takes it (`Harness::model_name`),
/// not described: an operator reading *"go back to deepseek"* would have to work out
/// which of five spellings the verb accepts, and the daemon already knows.
fn switch_back_said(previous: &str, now: &str, code: u16) -> String {
    format!(
        "the first round on `{now}` was refused by the provider (http {code}). Nothing this \
         turn produced is lost and the conversation is unchanged: `/models {previous}` takes \
         this session back to `{previous}`, and the next turn goes there."
    )
}

/// **The model a session switched away from**, kept until a round proves the new one
/// answers — the fact that makes a failed switch recoverable by the person who typed it.
///
/// Held in [`Harness::model_name`]'s spelling, which is the one `/models` takes, so the
/// command in the notice is not a second name to translate.
struct SwitchBack {
    /// What answered before the switch: `deepseek/deepseek-flash`, `local`.
    previous: String,
}

/// What a resume actually rebuilt.
///
/// Reported rather than assumed, and reported as *numbers*: "resumed" on its own is
/// the claim, and the row count, the token count and the chain head are the evidence.
/// The head in particular is what makes two resumes of the same session comparable —
/// if it differs, one of them is not the conversation the other was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeReport {
    pub transcript_id: String,
    pub rows: usize,
    pub tokens: usize,
    pub head: String,
    /// Where the tools are confined — this session's own root, which is not
    /// necessarily the directory the daemon was started in.
    pub workspace: String,
    /// Anything about this resume the operator would otherwise find out later: a
    /// workspace that moved, a stable prefix that no longer matches the daemon's.
    pub notes: Vec<String>,
}

/// **The operator's message when they hand a skill over** — `/skills NAME`, the pane's Enter.
///
/// The label is the whole of it: the body is somebody else's prose, and a model that read it as
/// the operator's own instruction would be following a text the operator merely pointed at. What
/// follows the name on their line goes on after it, as their words, in the same turn.
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
    /// **Record an operator's own call as their act** — R24 part two, decisions 2 and 4.
    ///
    /// The row this writes is what makes the difference between three facts a corpus must
    /// not confuse: *the guard allowed this*, *a rule allowed this*, and **the person ran it
    /// themselves**. `by` is `human:<who>` — the vocabulary the corpus already carries 121
    /// `human:dead` and 37 `human:leticl` rows of — `asked` is `true` because a person
    /// answered it (they are the answer), and `consulted` is `false` because no model spoke.
    /// That last field is what stops `agreement` reading it as the guard's success: the gate
    /// only labels a row against the model's verdict when a model gave one.
    pub fn admit_operator_call(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        who: &str,
    ) -> Result<(), String> {
        let Some(sink) = &self.corpus else {
            // **Not a silent success.** A daemon with no store cannot keep the record, and
            // a head told "admitted" would run a call whose admission is nowhere — which is
            // the one thing this arm exists to prevent.
            return Err(
                "this daemon keeps no corpus, so an operator's own call cannot be recorded \
                 as theirs. Refusing rather than admitting it unrecorded."
                    .into(),
            );
        };
        let request_id = format!("op-{call_id}");
        let row = letibot_tools::CorpusRow {
            request_id: request_id.clone(),
            session_id: self.hub.session_id(),
            turn_id: String::new(),
            action: format!("`{who}` ran `{name}` themselves, from this console"),
            trail: letibot_tools::AuthorisationTrail {
                utterances: Vec::new(),
                provenance: letibot_tools::authorise::TrailProvenance::Scanned {
                    messages_scanned: 0,
                    operator_messages: 0,
                },
            },
            shown: None,
            reply: None,
            shape: None,
            shape_class: None,
            baseline: String::new(),
            tier: "may_approve",
            tool: name.to_string(),
            arguments: serde_json::from_str(arguments)
                .unwrap_or(serde_json::Value::String(arguments.to_string())),
            mode: "head-run".into(),
            options: Vec::new(),
            agent: "operator".into(),
            model_verdict: None,
            verdict: Some("selected".into()),
            verdict_by: Some(format!("human:{who}")),
            verdict_basis: Some(
                "the operator ran this from their own console; there was nobody left to ask".into(),
            ),
            p_allow: None,
            decision_ms: 0,
            brief_format: "head-run",
            consulted: false,
            oracle_reading: None,
            effect: "admit",
            asked: true,
            operator: None,
        };
        sink.decided(&row);
        self.hub
            .publish(letibot_sessionlog::SessionEvent::OperatorCallAllowed {
                call_id: call_id.to_string(),
                name: name.to_string(),
                who: who.to_string(),
                arguments: arguments.to_string(),
            });
        Ok(())
    }

    /// **Append what the operator's call produced**, with its `origin` set.
    ///
    /// Through `append_imported`'s writer and no other, so the row lands in the ledger, the
    /// store and every head the same way a turn's rows do — and carries
    /// `origin: Some(Operator { who })`, which is the whole of decision 1: a head draws it as
    /// the person's act rather than the model's, and no head has to infer it.
    pub fn finish_operator_call(
        &mut self,
        call_id: &str,
        name: &str,
        who: &str,
        outcome: letibot_transcript::ToolOutcome,
        payload: &str,
    ) -> Result<(), HarnessError> {
        let item = TranscriptItem::ToolResult {
            call_id: call_id.to_string(),
            name: name.to_string(),
            outcome,
            payload: payload.to_string(),
            edit: None,
            origin: Some(letibot_transcript::CallOrigin::Operator {
                who: who.to_string(),
            }),
            media: None,
        };
        self.append_imported(&[item])
    }

    /// **Run an operator's admitted call, in this session, and put it in the transcript**
    /// — R31's `execute`.
    ///
    /// # Why the daemon runs it rather than the head
    ///
    /// Not convenience: **the payload has to be the one this program would have produced.**
    /// The corpus row for an operator's call says `human:<who>` ran `web_fetch`; a head that
    /// ran its own HTTP client would make that row false, and the model's next prompt would
    /// hold text `web_fetch` never returned. Here the tool is the seated one, so the byte
    /// caps, the spill policy, the network rules and the scratch directory are the same ones
    /// a model's call gets.
    ///
    /// **The gate is not consulted and must not be.** The operator's call has already been
    /// admitted against the door's allowlist and written as `human:<who>` — the row's own
    /// basis says *"there was nobody left to ask"*. Re-gating here would either double-record
    /// the decision or, at `/mode automode`, let a model refuse the person's own act.
    /// [`ToolRuntime::invoke_operator`] is that distinction made explicit rather than a flag
    /// at the call site.
    ///
    /// # The size, said before the row lands
    ///
    /// R31's fourth consequence: *"a 40k-token page is 40k of context the operator chose to
    /// buy — show the size before it lands, because the alternative is discovering it at the
    /// next compaction."* So the measurement is published as a note **between** the run and
    /// the append, which is the only window in which it is true: after the run the size is
    /// known, and before the append nothing has been added to what the model reads.
    pub fn run_operator_call(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        who: &str,
    ) -> Result<(), String> {
        let call = ToolCall {
            id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        };
        // The runtime's own events go to a null sink: the transcript row is the durable
        // record and every head draws *it*, so a second copy as an event would be §13.2b's
        // "one fact, two places". What the tool emits here is a `ToolStarted`/`ToolProgress`
        // pair for a call no head proposed.
        //
        // **Null, and WATCHED** — see [`Self::unprompted_sink`]. A call from this door can
        // return `Backgrounded` (`task` above all, and any tool the door's list grows to hold),
        // and an unwatched sink leaves that handle in nobody's table: the row promises a
        // completion and no watcher exists to queue one.
        let mut quiet = self.unprompted_sink();
        // `turn_id` is empty on purpose: this call belongs to no turn, and a head that saw a
        // turn id here would draw the row inside a turn that did not propose it.
        let result = self.runtime.invoke_operator("", &call, &mut quiet);
        // **What the model will read**, which is the number the operator is buying: the
        // payload is *already* what the spill policy left, so this is the post-cap figure and
        // not the tool's raw output. The full size, when it differs, is on the `SpillRef` the
        // runtime recorded — reported rather than recomputed, because a second implementation
        // of the cap is a second answer.
        let payload = result.render();
        let read = payload.len();
        let spilled = match &result.spill {
            Some(s) => format!(
                "; {} byte(s) were produced and the rest went to the spill store \
                 (`read_spill hash={}`)",
                s.full_bytes, s.hash
            ),
            None => String::new(),
        };
        // **Said before it is appended.** Not a refusal and not a gate — the call has already
        // been admitted — but the disclosure R31 asks for: the price is on the screen while
        // the operator can still do something about it, rather than at the next compaction.
        self.import_note(
            "operator_call_ran",
            format!(
                "`{who}` ran `{name}` from their own console: {read} byte(s) of context \
                 (about {} tokens) reach the model from its next turn{spilled}. No reply is \
                 generated — this is context, not a request.",
                read / 4,
            ),
        );
        // The row, with the outcome the tool gave and the size the note above promised.
        let outcome = result.outcome.clone();
        self.finish_operator_call(call_id, name, who, outcome, &payload)
            .map_err(|e| e.to_string())
    }

    /// **Run the operator's own shell line (`!`), and append BOTH halves as rows.**
    ///
    /// The `!` feature's daemon half. The frame's own docs (`ClientFrame::OperatorShell`)
    /// record why this is a frame of its own rather than the door; this is the run.
    ///
    /// # Why the daemon runs it, in one sentence the door already wrote
    ///
    /// [`Harness::run_operator_call`] runs the door's calls here "because the payload has to be
    /// the one this program would have produced" — and that is a fortiori true of a shell
    /// command, whose payload is bytes a program wrote. A head has no exec host, no confinement,
    /// no scratch directory and **no standing environment** — and the standing environment is
    /// what carries `LETIBOT_SOCKET`, `LETIBOT_SESSION` and `SUDO_ASKPASS`, without which `sudo`
    /// cannot ask anybody for a password. Running it anywhere but here is the feature not
    /// working.
    ///
    /// # The gate is not consulted, and neither is anything else that asks
    ///
    /// `ToolRuntime::invoke_operator` is the door's own ungated path — same schema lookup, same
    /// argument salvage, same byte caps, same spill policy, same `bash` execution path with its
    /// confinement and its `sudo` shim — and no `gate.admit`, no view-grant card. A session that
    /// does not seat `bash` (the default read-only seat) gets the runtime's own honest refusal
    /// as the result row, naming the tools it does have.
    ///
    /// # What lands, and in which order
    ///
    /// Two rows, one append: the operator's typed line as a `User` row with
    /// `speaker: Operator` — their words, verbatim, bang included, because `Speaker::Operator`
    /// means *the person at the keyboard* and that is exactly who typed it — then the result as a
    /// `ToolResult` named `bash` with `origin: CallOrigin::Operator`, which is the tree's existing
    /// vocabulary for *a person ran this call* and the row every head already draws with the
    /// tool-output treatment. The output is NOT speech: it is a program's bytes, reported, and
    /// giving it a `Speaker` would be pretending a shell spoke.
    ///
    /// # Why this synchronous composition still exists beside the split one
    ///
    /// [`Sessions::dispatch`] hands a between-turns `!` line to its own thread (see
    /// `crate::bangrun`) — a run that held the worker held every other session's work with it.
    /// Two paths still run the whole thing HERE, on the calling thread, and both are deliberate:
    /// the round boundary's pickup (`apply_queued_head_run`), whose whole point is that a turn
    /// already running reads the rows at its next round; and every caller that has no worker to
    /// hand back to — `letibot-m1`, and the tests that drive a harness directly. The pieces are
    /// shared with the split path ([`Self::prepare_operator_shell`],
    /// [`Self::settle_operator_shell`]), so there is one minting of the call id, one note, one
    /// row shape, whichever thread runs them.
    pub fn run_operator_shell(&mut self, line: &str, who: &str) -> Result<(), String> {
        let (call_id, call) = self.prepare_operator_shell(line)?;
        // The runtime's own events go to a null sink, exactly as the door's do: the
        // transcript row is the durable record and every head draws *it*. What the
        // tool emits here is a `ToolStarted`/`ToolProgress` pair for a call no head
        // proposed — and for a command the operator is watching the composer over.
        //
        // **And watched**, for the reason [`Self::unprompted_sink`] gives: a run moved to the
        // background mid-flight (Ctrl+O) settles as `Backgrounded`, and the row it leaves says
        // its completion will arrive on its own.
        let mut quiet = self.unprompted_sink();
        // `turn_id` empty on purpose, as the door's runs are: this call belongs to no
        // turn, and a head that saw a turn id would draw the row inside a turn that
        // did not propose it.
        let result = self.runtime.invoke_operator("", &call, &mut quiet);
        // The pieces, not the type: [`Self::settle_operator_shell`] takes what the split
        // path carries over the hub, and this path hands it the same shape.
        let spill = result
            .spill
            .as_ref()
            .map(|s| (s.full_bytes, s.hash.clone()));
        let outcome = result.outcome.clone();
        self.settle_operator_shell(line, who, &call_id, outcome, result.render(), spill)
    }

    /// **Mint the operator's own call: the `bang-<n>` id and the `bash` call it names.**
    ///
    /// The half of [`Self::run_operator_shell`] that needs the harness — the sequence counter
    /// (`bang_seq`) is this session's, so the id is minted HERE, on the worker, before the run
    /// is handed anywhere. What comes back is everything a thread that is not this one needs to
    /// run the command: a [`ToolCall`] whose payload is the line minus its bang, already
    /// validated by [`letibot_sessionlog::operator_shell_command`].
    pub fn prepare_operator_shell(&mut self, line: &str) -> Result<(String, ToolCall), String> {
        // **A pasted block is not a command, and the daemon checks this itself.** The composer
        // refuses it too, and that is not a second opinion: a `!` line reaches here from any
        // head that speaks the protocol, and the crate both halves share holds the rule. A
        // five-line notice used to become one `!` command whose newlines the shell split,
        // running `usage: sudo …`, `it.`, `so` and `/proc` as their own commands.
        if let Some(why) = letibot_sessionlog::operator_line_refusal(line) {
            return Err(why);
        }
        let command = letibot_sessionlog::operator_shell_command(line)
            .ok_or_else(|| format!("`{line}` is not a `!` command — nothing was run"))?
            .to_string();
        self.bang_seq += 1;
        let call_id = format!("bang-{}", self.bang_seq);
        let call = ToolCall {
            id: call_id.clone(),
            name: "bash".to_string(),
            arguments: serde_json::json!({ "command": command }).to_string(),
        };
        Ok((call_id, call))
    }

    /// **Append what the operator's run produced: the note and the two rows, one append.**
    ///
    /// The half of [`Self::run_operator_shell`] that needs the harness again after the run —
    /// which is why it runs on the worker even when the run did not: the transcript has one
    /// writer, and this is it. Everything here is byte-for-byte what the synchronous path
    /// always did, in the same order: the size note first (so the disclosure is on the log
    /// before the rows that carry the bytes), then the `User` row with the typed line verbatim,
    /// then the `bash` `ToolResult` with `origin: CallOrigin::Operator`.
    ///
    /// It takes the run's pieces rather than a [`letibot_tools::ToolResult`] because the
    /// split path's result crosses a thread as [`CommandKind::OperatorShellResult`] —
    /// already rendered and already carrying its spill numbers, so the worker never needs
    /// the type back. The synchronous path renders at its own call site; both arrive here
    /// identical.
    pub fn settle_operator_shell(
        &mut self,
        line: &str,
        who: &str,
        call_id: &str,
        outcome: letibot_transcript::ToolOutcome,
        payload: String,
        spill: Option<(usize, String)>,
    ) -> Result<(), String> {
        // The command as the note names it: the line minus its bang, by the same validator
        // the frame and [`Self::prepare_operator_shell`] used. The `None` arm is not a
        // silent default — it is the caller's own claim carried through: a line settled
        // without a bang was still run as written, and the note says so rather than
        // inventing a command nobody typed.
        let command = letibot_sessionlog::operator_shell_command(line)
            .map(str::to_string)
            .unwrap_or_else(|| line.to_string());
        // **What the model will read** — the same disclosure the door's note makes,
        // for the same reason: the price is on the screen while the operator can still
        // do something about it, rather than at the next compaction.
        let read = payload.len();
        let spilled = match &spill {
            Some((full_bytes, hash)) => format!(
                "; {full_bytes} byte(s) were produced and the rest went to the spill store \
                 (`read_spill hash={hash}`)"
            ),
            None => String::new(),
        };
        self.import_note(
            "operator_shell_ran",
            format!(
                "`{who}` ran `{command}` from their own console: {read} byte(s) of context \
                 (about {} tokens) reach the model in the turn this line starts{spilled}. \
                 The turn is the sending half — the operator asked for their command's \
                 result to be *sent to the model as a message*, so a reply is generated.",
                read / 4,
            ),
        );
        let rows = [
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: line.to_string(),
                }],
            },
            TranscriptItem::ToolResult {
                call_id: call_id.to_string(),
                name: "bash".to_string(),
                outcome,
                payload,
                edit: None,
                origin: Some(letibot_transcript::CallOrigin::Operator {
                    who: who.to_string(),
                }),
                media: None,
            },
        ];
        self.append_imported(&rows).map_err(|e| e.to_string())
    }

    /// **Write down the question the model asked and the answer a person gave it.**
    ///
    /// Called from the round loop with the result of an [`ASK_USER_QUESTION`] call — *the*
    /// place a result is appended, and the reason the tool does not write these rows itself:
    /// the transcript has one writer, and it is the session. What the tool returns is the
    /// answer the model reads; what a PERSON said is written here, as their own row.
    ///
    /// # What lands, and in which order
    ///
    /// The question as the session's own row ([`question_row_text`] — never the operator's,
    /// because the trail may cite a `User` row of theirs and a question is not theirs), then
    /// the answer as a `User` row with `speaker: Operator` whose text is **exactly** what
    /// they said ([`operator_answer_text`]).
    ///
    /// # Why they land AFTER the call's own result
    ///
    /// The brief's shape — the person's row, then the result — is the `!` line's, and it is
    /// right there because nothing above the result proposed the call. Here something did:
    /// the model's `Assistant { tool_calls }` row is already on the log, and
    /// `letibot_provider::messages::pair_tool_calls` **closes the answering window on any
    /// non-`tool` row**. A `User` row between the proposal and its result therefore makes
    /// the model read *"no result was recorded for this call"* beside the real one, which is
    /// a lie about the exchange the rows exist to record. So the result goes first and the
    /// person's half follows it — the round loop appends both in one batch, all results
    /// before all of these.
    ///
    /// # What makes the result the person's act
    ///
    /// The row keeps its payload — the model reads the same bytes it always did — and takes
    /// `origin: Some(CallOrigin::Operator { who })`, which is what makes every head draw it
    /// as the person's act at every verbosity (`tui::app::visibility::operator_act`). The
    /// operator's report this answers is *"by the way i dont see my answer to the selector"*:
    /// the card is a live overlay and this row is what is left of it afterwards.
    ///
    /// # The trail
    ///
    /// Both rows are recorded where the composer's own speech is (`TrailMirror::say`), and
    /// the SPEAKER is the whole point: the answer goes in as `Speaker::Operator`, so a *yes*
    /// given through a question card can be cited as authorisation exactly as a *yes* typed
    /// into the composer can. Before this it could not — the answer was a tool payload and
    /// nothing else, and no `User` row of theirs existed to cite.
    fn settle_asked(&mut self, result: &mut TranscriptItem) -> Vec<TranscriptItem> {
        let Some(asked) = self.asked.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Vec::new();
        };
        // **Only an answer the tool ACCEPTED.** `ask_user_question` reports an abstention as
        // `Abstained` — a real answer — and everything it refused (an empty answer, an option
        // index nobody offered, a note qualifying nothing) as `not_run`. Writing a person's
        // row for one of those would attribute to them a sentence the harness rejected.
        let TranscriptItem::ToolResult {
            outcome, origin, ..
        } = result
        else {
            return Vec::new();
        };
        if !matches!(
            outcome,
            letibot_transcript::ToolOutcome::Ok | letibot_transcript::ToolOutcome::Abstained { .. }
        ) {
            return Vec::new();
        }
        let Some(said) = operator_answer_text(&asked) else {
            return Vec::new();
        };
        *origin = Some(letibot_transcript::CallOrigin::Operator {
            who: asked.by.clone(),
        });
        let at = Some(Instant::now());
        self.trail
            .say(Speaker::Agent, &question_row_text(&asked), at);
        self.trail.say(Speaker::Operator, &said, at);
        asked_rows(&asked, &said).to_vec()
    }

    /// **The runtime as a shareable handle**, for the thread that runs the operator's
    /// command. One exec host per session is a fact — `job_list`, a promotion and the
    /// run ender at shutdown all name jobs in this one — so the run goes through THIS
    /// runtime rather than a second one, and the clone is the whole of the handover.
    ///
    /// **The sink is the caller's, and it decides whether a `Backgrounded` outcome is
    /// watched.** A runtime does not know what a watcher is, so a handle used with a bare
    /// `NullToolSink` mints a job or subagent handle in nobody's table: the settlement is
    /// queued for no session and no bell rings. Whoever invokes through this must pass
    /// [`JobWatchSink::new`] over [`Harness::job_watchers`] — which is what
    /// [`Harness::unprompted_sink`] is for the two doors that live here, and what
    /// `crate::bangrun` carries across its thread.
    pub fn runtime_handle(&self) -> std::sync::Arc<ToolRuntime> {
        self.runtime.clone()
    }

    /// **Attach the worker's serialization state** — the same `Arc` the dispatch arm
    /// holds, so both places that could start an operator run read one fact. Called by
    /// `Sessions` at open, once per harness; a second call would replace the state and
    /// orphan any run in flight, which is why it is a setter rather than an argument:
    /// the harness can be built before the map that owns the state exists.
    pub fn set_bang_state(&mut self, state: std::sync::Arc<crate::bangrun::State>) {
        self.bang = Some(state);
    }

    /// Whether an operator run for this session is in flight on its own thread. The
    /// mid-turn pickup's question; `false` when no state was attached, which keeps the
    /// bare-harness callers on the synchronous path they always had.
    fn bang_in_flight(&self) -> bool {
        self.bang.as_ref().is_some_and(|s| s.in_flight())
    }

    /// Park a `!` line typed mid-turn while a run is in flight: it starts when that
    /// run settles, on the worker, between turns. Only called when [`Self::bang_in_flight`]
    /// answered true, so the state is there to park into.
    fn park_bang_line(&self, line: String, who: String) {
        if let Some(s) = &self.bang {
            s.queue(line, who);
        }
    }

    /// **The turn the operator's own `!` line starts** — the sending half.
    ///
    /// [`Harness::run_operator_shell`] appends the two rows; this is what makes them a
    /// message rather than silent context. The operator asked for both halves in their own
    /// words — the result *"printed (with meaningfull truncution and scrolled collapse,
    /// like we do for tool output now) and sent to model as a message"* — and corrected
    /// the design when the rows landed without the turn: *"my commands should start a turn
    /// and should be printed to me"*.
    ///
    /// **Nothing is appended here, on purpose.** The line and its output are already rows
    /// and already on the log, so a framing item would be a third row saying what the two
    /// above it say. This is `submit_item` minus its append: open a turn, run the rounds,
    /// and the model reads the operator's line and the command's output as the head of
    /// that turn.
    ///
    /// **Called only between turns.** Mid-turn the line is taken at the round boundary
    /// (`apply_queued_head_run`) and the turn already running reads the rows at its next
    /// round; starting one there would be a second turn about one command.
    pub fn run_after_operator_shell(&mut self) -> Result<Reply, HarnessError> {
        self.trail.begin_turn();
        let out = self.run_rounds();
        self.note_turn_ended();
        out
    }

    /// Append imported rows through the one writer, exactly as a turn's rows go in.
    fn append_imported(&mut self, items: &[TranscriptItem]) -> Result<(), HarnessError> {
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session.append_items(&self.engine, items, &mut sink)?;
        self.reconcile(&mut sink, items);
        Ok(())
    }

    /// A warning on this session's log — the one place an import reports itself.
    fn import_note(&self, code: &str, detail: String) {
        self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
            code: code.to_string(),
            detail,

            compaction: None,
        });
    }

    /// The turn this harness last started, or empty before the first one.
    ///
    /// The daemon needs it to publish `TurnFailed` against the turn a head is
    /// watching. Empty is a real answer — a turn that failed while being *set up*
    /// never got an id — and the head treats an empty one as "the pane you are
    /// looking at", which is the only pane that can be spinning at that point.
    pub fn last_turn_id(&self) -> &str {
        &self.last_turn_id
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// **The cell the daemon fills with this session's plugin host** — see the field.
    ///
    /// `Sessions::open_plugins` is the only caller: it builds the host (the one place a hub and a
    /// pane driver exist) and fills this in, so the `plugin` tool seated when this harness opened
    /// asks the very stack the head's panes are drawn from. Two hosts would be two answers to
    /// *what is in force*.
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

    /// The project root this session's tools are confined to — the session's own,
    /// resolved from the store at open, not necessarily the directory the daemon
    /// started in. The `/mode` command keys the per-project store on it.
    pub fn workspace(&self) -> &std::path::Path {
        &self.cfg.workspace
    }

    /// **This session's connection to the box's room**, when it has one — the `room` tool's half,
    /// and what a room pane is drawn from (`crate::plugins::room_feed`).
    ///
    /// It is `None` on a box with no coordinator and on a subagent (which speaks through its
    /// parent), and both of those are ordinary states rather than failures.
    pub fn room_link(&self) -> Option<Arc<crate::room::link::SessionRoom>> {
        self.room.clone()
    }

    /// **Turn the guard model on or off, now, on the running session.**
    ///
    /// > *"I want to start leticode, do /supervise, and move on."*
    ///
    /// Reaches the live gate rather than persisting a setting for next time, which is
    /// the difference between this and `/mode`: a mode decides which tools are seated
    /// and what asks, so it is fixed when a session opens; supervision decides only
    /// whether the model gets a turn before the answer, and nothing seated depends on
    /// that.
    ///
    /// Returns the gate's own sentence, refusal included. A session with no
    /// `--oracle` gets a refusal naming the flag — never a quiet success.
    /// Whether the guard model currently gets a turn. Read off the gate, never off
    /// the config that asked for it.
    pub fn supervising(&self) -> bool {
        self.gate().supervising()
    }

    /// The gate, locked. See `ToolRuntime::gate` for why it is behind one: the runtime
    /// is shared with the operator's run thread, and the gate is the one field that
    /// mutates in place. Held across a decision by the caller that makes one; these
    /// reads and moves are momentary.
    fn gate(&self) -> std::sync::MutexGuard<'_, Box<dyn letibot_tools::Gate>> {
        self.runtime.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    // Point this session's guard at `endpoint`, build it and attach it.
    //
    // For `/supervise HOST:PORT`, which is the override and not the normal path —
    // the normal path is `[gatekeeper] endpoint` in the operator's config, read at
    // daemon start. Nothing is written here: config is the operator's file to edit,
    // and a harness that rewrote it behind them would make a one-off override
    // permanent without being asked.

    /// **Close out tool calls no round owns any more, and look in the logs to say why.**
    ///
    /// MEASURED, on the operator's own head: a `grep` was dispatched, its executor thread and its
    /// process both vanished, and the call sat `running` in the head for eight minutes while
    /// `esc esc` did nothing — because the interrupt stops GENERATION and generation had long since
    /// ended. Nothing in this daemon looked. There were three callers of `Job::settle` and every one
    /// of them was the thing that was already gone.
    ///
    /// # Why this is a CHECK and not a timeout
    ///
    /// The round loop is SYNCHRONOUS: a turn calls `runtime.invoke` and blocks until the tool
    /// answers, so while a round is in flight **this function cannot run at all** — it runs on the
    /// worker thread, which is the thread the round is holding. So if it is running, no round owns
    /// any call, and a call with no result in the transcript is a call nothing will ever answer.
    ///
    /// That is the whole of the liveness test, and it needs no clock, no thread introspection and no
    /// new bookkeeping: **the fact that we are here is the fact that the executor is gone.** A
    /// timeout would have to guess a duration that is generous to `cargo build` and still quick for
    /// a lie; this cannot be wrong about a call that is legitimately in flight, because a
    /// legitimately-in-flight call is one this cannot run alongside.
    ///
    /// # And it reads the log, because "failed" is not a diagnosis
    ///
    /// The operator: *"if it is dead let it look into the logs."* The stored transcript is the
    /// durable record, and the rows around the stall are what a reader needs — what the run was
    /// doing, which call went unanswered, and what came before it. So the payload carries the tail
    /// of the session's own stored transcript, newest last, and the warning names the call.
    ///
    /// A `NotRun` outcome rather than `Failed`, deliberately: nothing ran and nothing failed. F5's
    /// rule — *a component's "I did not do this" must not be reported as a success* — cuts the other
    /// way here too, and the honest word for a call whose executor disappeared is that it did not
    /// run.
    pub fn sweep_abandoned_calls(&mut self) -> usize {
        // **THE DECISION IS A FREE FUNCTION**, so it can be tested without a daemon, a model or a
        // socket. What is left here is the plumbing: read the log, build the rows, append them.
        let abandoned = abandoned_calls(self.session.items.as_slice());
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
        // **And the head is told, and told when the record could not be written** — R67 item 15.
        // This used to be a hub block that did nothing (`let _ = hub;`) and a `let _ =` on the
        // append, so a call closed out here was announced to nobody but the transcript, and a
        // close-out the store refused was announced to nobody at all: the spinner the sweep exists
        // to end would stay up, and nothing would say why.
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

    pub fn attach_oracle(&mut self, endpoint: Endpoint) -> Result<String, String> {
        self.cfg.oracle = Some(endpoint.clone());
        let advisor = model_adjudicator(&self.cfg, "`/supervise`", Some(self.hub.clone()))
            .map_err(|e| e.to_string())?;
        self.gate().attach_advisor(std::sync::Arc::from(advisor))?;
        Ok(format!(
            "guard model at {} for this session. To make it the default, put it in \
             {}:\n  [gatekeeper]\n  endpoint = \"{}\"",
            endpoint.authority(),
            letibot_provider::keys::config_file().display(),
            endpoint.authority()
        ))
    }

    /// **Move THIS session to another point, now.**
    ///
    /// `/mode` wrote the project's row and told the operator the point applied
    /// "from the NEXT session". That was true, and it was asked about three times,
    /// because the sentence is not what anybody typing `/mode automode` wants: they
    /// want the session in front of them to behave differently from the next call.
    ///
    /// What the gate needs is a field write — it reads its mode at decision time —
    /// so the work here is the part the old comment worried about: the prerequisite
    /// check the open would have run, re-run against what this session actually
    /// has; the standing grants, dropped (a new point is a new question); and a
    /// point whose decider is the model getting its supervision turned on, the way
    /// an open at that point would. Nothing here touches the backend: writable or
    /// confined is a property of the SEAT, and the prerequisite check is what
    /// refuses a point the seat cannot carry.
    pub fn set_mode(&mut self, mode: letibot_tools::mode::Mode) -> Result<String, String> {
        self.set_mode_consented(mode, false)
    }

    /// **`/mode`, with the operator's answer to the unconfined-`allow-all` question.**
    ///
    /// `allow-all` requires a confinement and `backend_confined` is true for a
    /// firecode placement and nothing else, so on the operator's own box the point
    /// was unreachable by every route and the refusal pointed at cgroups and bwrap,
    /// which could not have supplied it. Their words, 2026-09-20: *"allow-all doesnt
    /// work"*, and then *"make it ask for confirmation on bare host and let it thru"*.
    ///
    /// So: the head asks, and a `yes` arrives here as `consented`. It selects
    /// [`Mode::ALLOW_ALL_HERE`] — the same coordinate with the boundary told
    /// truthfully — and only under all three of these, each checked rather than
    /// assumed:
    ///
    ///   1. the point asked for is `allow-all`. Consent is for one question; it does
    ///      not travel to a point the operator did not pick.
    ///   2. this session really has no confinement. Inside a VM the ordinary
    ///      `allow-all` already opens, and substituting the consented point there
    ///      would record a person's answer where a structural fact was the reason.
    ///   3. the substituted point passes `check` on its own. It drops only
    ///      `Confinement`; a seat with no writable backend still cannot carry it.
    ///
    /// **It is not persisted.** `sessions.rs` writes the project's row from the name
    /// the operator typed, which stays `allow-all` and stays refused for the next
    /// daemon. Consent is a thing a person gave once, in front of one session; a
    /// consented point that came back on every later start would be exactly the
    /// "banner says one thing" state this module refuses everywhere else.
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
            // No `supplies` means the open never ran the check — a session built by
            // a test harness. Consent cannot be honoured against a list nobody
            // computed, so it is refused rather than granted on a guess.
            return Err("this session did not record what it can supply, so an \
                        unconfined `allow-all` cannot be confirmed against it"
                .into());
        }
        let dropped = self.gate().set_mode(mode)?;
        let mut said = format!("this session is at `{}` from the next call", mode.name);
        if vouched {
            // Said here rather than only in the confirmation the head showed: the
            // banner, the warning on the log and this sentence are what a person
            // reads later, and "you agreed to this" belongs in all three.
            // Not "the project's row still reads `allow-all`" — it does not.
            // Nothing was written, so the row reads whatever it read before, and
            // claiming otherwise put a third false sentence on a card that already
            // had two.
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
        // **And the spawn path's copy of it**, which is a different object: the runner
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

    /// **Does the point asked for need a PERSON's answer here** — the two facts
    /// [`Harness::set_mode_consented`] reads from `supplies`, asked on their own.
    ///
    /// `allow-all` requires a confinement, and a session on a bare host has none; that is the
    /// one point in the tree whose opening is a person's answer rather than a structural fact
    /// (`Mode::ALLOW_ALL_HERE`). A session that never recorded what it can supply — a fixture
    /// — is answered `true`, the fail-closed direction, because consent cannot be honoured
    /// against a list nobody computed.
    pub fn mode_needs_consent(&self, mode: letibot_tools::mode::Mode) -> bool {
        if mode.name != letibot_tools::mode::Mode::ALLOW_ALL.name {
            return false;
        }
        match &self.supplies {
            Some((have, _)) => !have.contains(&letibot_tools::mode::Prereq::Confinement),
            None => true,
        }
    }

    /// **A head ASKS for the point that needs a person** (`PROTOCOL_VERSION` 54, B8).
    ///
    /// The bridge can want `allow-all` for a package's session and cannot be the person whose
    /// box is the boundary, so it asks instead of setting: the confirmation is pushed to the
    /// session's OTHER heads that may decide, and the point moves only when one of them says
    /// yes ([`Harness::answer_mode_confirm`]).
    ///
    /// Three things this decides, and they are the whole of what the requester is told:
    ///
    /// * **The point needs nobody's confirmation** — `writes-allowed`, `read-only`, a VM-placed
    ///   `allow-all` — so it is applied here and settled `applied`. A program asking for a point
    ///   no person has to approve is not a question for a person.
    /// * **Nobody can be asked** — the session's only head is the one that asked. Then the answer
    ///   is the refusal the bridge returns to its caller: *needs the operator's confirmation in a
    ///   head: run `leticode --session <id>`*, which is the command that opens one.
    /// * **Otherwise the card goes out** and this returns with the request open.
    ///
    /// **One open request per session, and a new one supersedes it** — the alternative is a
    /// requester whose own patience ran out leaving a card up that refuses every later attempt.
    /// The superseded request is settled `answer: nobody, applied: false`, so the head that was
    /// asked takes the stale card down before the new one lands.
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
            // **The same `mode_set` line the `/mode` path writes**, because a point that moved
            // has to be legible afterwards on the session it moved in — and this one moved
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
            // **Never the head that asked.** A program that could answer its own request
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

    /// **A head's answer to that ask** — `yes` applies the point, `no` applies nothing.
    ///
    /// Refused by name when no request is open, when the id is not the open one, and when this
    /// head was not asked. That last check is the structural half of *the bridge never consents
    /// for itself*: the daemon sent the card to a set of heads, and only those heads can answer
    /// it.
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
                // **A yes the session cannot carry is not applied**, and the person is told by
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

    /// **How a confirmation ended, to every head** — the asker's answer and the other heads'
    /// dismissal of the card, in one frame.
    ///
    /// `name` is **the point that is now in force** when it applied, which is not always the
    /// point that was asked for: a consented `allow-all` on a bare host lands on
    /// [`letibot_tools::mode::Mode::ALLOW_ALL_HERE`], the same coordinate with the boundary
    /// told truthfully. A caller that read `allow-all` back would be told the point it asked
    /// for rather than the one its session is at.
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

    /// Push the settings this session runs under to the registry, where the
    /// server answers a head's `Settings` from. On open and on every runtime
    /// change, so the pane never shows a mode that was true a minute ago.
    fn publish_settings(&self) {
        let mut rows = self.cfg.settings(
            &self.mode_source,
            self.gate().supervising(),
            &self.door_tools(),
        );
        // **Whether the board waits on the operator** — `none`, or how many and the first
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

    /// **What a head needs to offer the door without holding a schema** — R31 and R32.
    ///
    /// Derived here, from the registry this session seats and the allowlist the daemon
    /// enforces, so a tool whose schema changes moves a head's behaviour with no head
    /// rebuilt. A name the allowlist carries and the registry does not comes back with the
    /// sentence a head says instead of the bare form, which is a visible miss rather than a
    /// silent absence.
    ///
    /// **Idempotent and cheap**: this runs on open and on every republish, and it is one
    /// pass over three schemas.
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
        // **Attach on demand.** `/supervise` on a session that opened with no
        // `--oracle` is the ordinary case, not the exceptional one: nobody types
        // daemon flags. If an address is known from anywhere, use it rather than
        // refusing and sending the operator to restart a daemon.
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

    /// **Where this session's point came from** — the settings row's own field, for a
    /// caller that wants to say it somewhere else as well (the spawn does: a child whose
    /// point was clamped says so on its parent's progress line). Empty meanings are the
    /// row's; see [`Config::settings`].
    pub fn mode_source(&self) -> &str {
        &self.mode_source
    }

    /// The session's own config, to change a decision that is the session's
    /// rather than the daemon's — `auto_compact` turning itself off after a
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

    /// **What this session's harness holds, now** — the same answer the hub's census gets.
    ///
    /// Read by a test that wants to compare the census against the transcript it describes, and
    /// by nothing in a daemon (the daemon reads it through the hub, on the watchdog's thread).
    /// The transcript's half is the gauges, which [`restate_footprint`](Self::restate_footprint)
    /// keeps exact; the trail's is read live.
    pub fn footprint(&self) -> letibot_sessionlog::hub::Footprint {
        crate::footprint::of(&self.footprint_gauges, &self.trail)
    }

    /// **Restate the hub's footprint from what this harness holds** — see [`crate::footprint`].
    ///
    /// Called at open and from [`Harness::reconcile`], which every append goes through. Each
    /// call walks the rows: an increment would be cheaper and would under-report for ever after
    /// one missed site, and a transcript is a few hundred rows of a few strings each.
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

    /// The session's todo list as the model last wrote it — the board's
    /// snapshot, which is what a resume restored and what `flush_todos` keeps
    /// durable.
    pub fn todo_list(&self) -> Vec<TodoItem> {
        self.todos.snapshot()
    }

    /// **This session's watcher set** — what queues a background job's or a subagent's
    /// settlement for THIS session, whichever transcript it is speaking by then.
    ///
    /// A turn arms it through its own sink (`JobWatchSink`); this is for a caller that invokes
    /// a tool outside a turn and still wants the `Backgrounded` outcome watched the same way.
    /// The `/clear` test is that caller: it starts the job and the child through the runtime,
    /// clears, and proves the settlements land in the new conversation.
    pub fn job_watchers(&self) -> Option<Arc<JobWatchers>> {
        self.job_watch.clone()
    }

    /// **The sink for a call this session runs OUTSIDE a turn** — the operator's own call and
    /// their `!` line, both of which run on a door rather than in a round.
    ///
    /// It is a [`JobWatchSink`] around [`letibot_tools::events::NullToolSink`], and both halves
    /// are load-bearing:
    ///
    /// * **null**, because the transcript row is the durable record and every head draws *it* —
    ///   what the tool emits here is a `ToolStarted`/`ToolProgress` pair for a call no head
    ///   proposed, and a second copy as an event would be one fact in two places;
    /// * **watched**, because a `Backgrounded` outcome from such a call is a promise the row
    ///   makes in words — *its completion will be delivered to you on its own* — and nothing
    ///   delivers it unless the watcher was armed where the handle was minted.
    ///
    /// MEASURED 2026-10-31: both doors invoked with a bare `NullToolSink`, so an operator's run
    /// that was moved to the background (Ctrl+O mid-flight) or a `task` run from the console
    /// queued no settlement and rang no bell. The row said the completion would arrive; it never
    /// could. The same fix at `crate::bangrun`'s thread, which is where a `!` line between turns
    /// actually runs.
    fn unprompted_sink(&self) -> JobWatchSink<letibot_tools::events::NullToolSink> {
        JobWatchSink::new(letibot_tools::events::NullToolSink, self.job_watch.clone())
    }

    /// **The board's version — the one fact that says the board moved, and from whom.**
    ///
    /// `flush_todos` watches it to decide whether an announcement is owed; a parked
    /// child's serving loop (`serve_child`) watches it for the other writer that can
    /// move this board: a parent's `todo_write target=` (`child_todos`), which runs on
    /// the parent's thread and leaves this the only way to notice. The version is the
    /// board's own (bumped by every mutator, whatever thread it ran on), so it needs no
    /// new channel and cannot disagree with the flush.
    pub fn todo_board_version(&self) -> u64 {
        self.todos.version()
    }

    pub fn ledger_head(&self) -> String {
        self.session.ledger_head()
    }

    /// The ledger's whole token count — prefix and body together — which is the
    /// number a compaction is measured against.
    pub fn ledger_len(&self) -> usize {
        self.session.ledger.len()
    }

    /// Whether this session's turns go to a metered provider — the case where the
    /// ledger and the provider's count are two different numbers.
    pub fn is_metered(&self) -> bool {
        self.provider.is_some()
    }

    /// **The unit a printed count is in**, for the lines that print one: a count with
    /// no unit beside another in a different one is how *"3191928 of 3404045"* and
    /// *"937686 of 999999"* came to sit on adjacent log lines for the same check (R72).
    ///
    /// Three answers on a metered session, because there are three states and only one of
    /// them is a measurement: `deepseek tokens` (measured on this model), `glm-coding tokens
    /// (estimated with deepseek/deepseek-flash's measured scale)` (the ratio a switch carried),
    /// and `ledger tokens (unscaled …)` (nothing measured anywhere).
    pub fn token_unit(&self) -> String {
        match (&self.provider, self.cfg.ledger_scale) {
            (Some(p), Some(_)) => format!("{} tokens", p.name()),
            // **An estimate names the model that made it.** `estimated with deepseek's
            // measured scale` is a different claim from `deepseek tokens` and from
            // `ledger tokens (unscaled …)`, and a reader who is not told which one they are
            // looking at cannot tell a measurement from an inherited ratio.
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

    /// The stable prefix's tokens, which the ledger keeps apart from the item rows.
    ///
    /// C9 compares against exactly this: a system change that appended left these
    /// bytes untouched, and one that rewrote the head did not.
    pub fn prefix_tokens(&self) -> &[letibot_tokencore::TokenId] {
        self.session.ledger.prefix_tokens()
    }

    /// The ledger row ids, in order.
    ///
    /// What pairs an announcement with its body: `TranscriptAppended` carries an id
    /// and `Hub::record_item` fills the row it names, and a second minting rule
    /// anywhere would go stale against `Session::append_items`. A caller checking
    /// that pairing needs to read the ids rather than recompute them.
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

    /// **The loop.** One user message in, one answer out, however many tool rounds
    /// that takes.
    ///
    /// This is the one door the **operator's own words** come through, and it is the
    /// only place that records them as such: see [`TrailMirror`] for why the speaker
    /// cannot be recovered from the transcript afterwards.
    pub fn submit(&mut self, text: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken(Speaker::Operator, text)
    }

    /// **The operator's words with the chips they put beside them** — `PROTOCOL_VERSION` 50.
    ///
    /// The files are read HERE, by the daemon, because the daemon is the half that writes the
    /// transcript: the row is the durable artifact, and what it carries is the `data:` URI and
    /// the inlined text rather than a path that may be gone by the time the next round renders
    /// the conversation (`letibot_transcript::media`'s own argument for putting `Media` on a
    /// row). See [`crate::attachments`] for what each kind becomes.
    ///
    /// A file that could not be read is said in the row and warned on the log — the words still
    /// send, because they are the operator's and the attachment is not — and an EMPTY list is
    /// [`Harness::submit`], byte for byte.
    pub fn submit_attached(
        &mut self,
        text: &str,
        attachments: &[letibot_sessionlog::protocol::Attachment],
    ) -> Result<Reply, HarnessError> {
        if attachments.is_empty() {
            return self.submit(text);
        }
        let (extra, refused) = crate::attachments::parts(attachments);
        for said in &refused {
            self.warn("attachment_refused", said.clone());
        }
        self.submit_spoken_parts(Speaker::Operator, text, extra)
    }

    /// **A warning on this session's log** — the operator's own sentence about something that
    /// did not go as asked. One writer for the two doors that can refuse an attachment.
    pub(crate) fn warn(&self, code: &str, detail: String) {
        self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
            code: code.into(),
            detail,
            compaction: None,
        });
    }

    /// **One turn, with the speaker given** — the operator's own words through
    /// [`Harness::submit`], a parent's correction through
    /// [`Harness::submit_a_parents_message`].
    ///
    /// The speaker is not decoration. [`AuthorisationTrail`] is where a turn's authority is
    /// decided and [`Speaker::Agent`] never authorises on its own, so a row that says the wrong
    /// speaker is a row that grants the wrong thing — which is why the parent's message has a
    /// door of its own rather than a flag on this one. The two speaker vocabularies are matched
    /// inline here for the same reason `TrailMirror::seed` matches them inline: the one place
    /// they meet is the one place a reader has to look for the mapping.
    fn submit_spoken(&mut self, speaker: Speaker, text: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken_parts(speaker, text, Vec::new())
    }

    /// The same, with the parts an attachment became — see [`Harness::submit_attached`].
    fn submit_spoken_parts(
        &mut self,
        speaker: Speaker,
        text: &str,
        extra: Vec<UserPart>,
    ) -> Result<Reply, HarnessError> {
        self.trail.begin_turn();
        // **R79: the results held for this break ride in with the turn** — as the harness's
        // own row, BEFORE the words that started the turn, so what the operator (or a parent)
        // said is still the last thing the model reads.
        let mut items = Vec::new();
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            self.trail
                .say(Speaker::Agent, &meanwhile, Some(Instant::now()));
            items.push(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text: meanwhile }],
            });
        }
        self.trail.say(speaker, text, Some(Instant::now()));
        items.push(TranscriptItem::User {
            // The same speaker the trail records, one line up (R42).
            speaker: match speaker {
                Speaker::Operator => letibot_transcript::Speaker::Operator,
                // A session talking to itself, a tool's row, a parent's message: none of
                // them is the operator's words, and none may be drawn as them.
                Speaker::Agent | Speaker::Tool => letibot_transcript::Speaker::Agent,
            },
            // **The words first and whole, the chips after** — a head retires its `queued`
            // echo by the row's leading lines, so nothing may be spliced in front of them.
            parts: {
                let mut parts = vec![UserPart::Text { text: text.into() }];
                parts.extend(extra);
                parts
            },
        });
        self.submit_items(items)
    }

    /// **R79's delivery for this session** — see [`Deliveries`]. `None` for a session with no
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

    /// **The operator ends a monitor** — the monitors pane's `x`, as `/monitor retire NAME`. The
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

    /// **Switch to a model this fleet hosts** — `/models dense78`, `/models local.glm`.
    ///
    /// Declared in `providers.toml` as a `[model."…"]` block with an address; see
    /// [`letibot_provider::keys::LocalModel`]. These are `local` in every sense that
    /// matters here: no key, no meter, an OpenAI-compatible or llama.cpp server on the
    /// LAN. So this takes the local path — `provider` cleared — and moves where that
    /// path points.
    ///
    /// # Why it verifies the vocabulary before it moves
    ///
    /// The engine tokenizes HERE, with this daemon's GGUF, and sends token ids. Point
    /// it at a server holding different weights and nothing fails: the ids are valid
    /// numbers and mean other words. That is silent corruption of the one thing a
    /// transcript is for, so a switch that cannot show the weights match does not
    /// happen — unless it does something better than refuse: load the vocabulary of
    /// the weights the target itself names, seat the dialect the target's own
    /// template settles on, and re-render the conversation into a fork under both.
    ///
    /// Four outcomes, and each is a sentence rather than a shrug:
    ///
    ///   * `/props` names the same GGUF this daemon tokenizes with — the address
    ///     moves, and nothing else does. The engine, the dialect and the
    ///     conversation's tokens all stay.
    ///   * `/props` names OTHER weights whose template matches a dialect this tree
    ///     drives (or the block asserts one) — a full switch: their vocabulary
    ///     loaded from the GGUF they name, their dialect seated, the conversation
    ///     re-rendered into a fork keyed by that dialect's template, the window
    ///     retuned against the box that will answer. `/models local.glm` from a
    ///     qwen daemon is this arm, and it is the arm the operator asked for.
    ///   * the weights differ and no dialect can be named honestly — refused,
    ///     naming what was looked for and the key that asserts it by hand.
    ///   * nothing answers, or `/props` names no model at all (a proxy, a server
    ///     without it) — refused, and `same_vocab = true` in the block asserts by
    ///     hand that the weights match. An operator saying *"I know these are the
    ///     same weights"* is a decision on the record; this guessing it would not be.
    ///
    /// # Why the cross-weights arm forks rather than swapping the renderer under a
    /// live transcript
    ///
    /// Every token in the session's ledger was rendered by one dialect against one
    /// vocabulary. Appending another dialect's bytes to them builds a prompt no
    /// model was trained on — the exact corruption the vocabulary check exists to
    /// refuse, one layer down. So the switch re-renders the conversation whole,
    /// the same way a resume into a daemon of another dialect does, into a fork
    /// whose stable-prefix row is keyed by the NEW dialect's template sha: another
    /// dialect's cache row is never reused, by construction rather than by check.
    pub fn set_local_model(
        &mut self,
        m: &letibot_provider::keys::LocalModel,
    ) -> Result<String, HarnessError> {
        let want = parse_local_url(&m.url)
            .map_err(|e| HarnessError::Setup(format!("[model.\"{}\"] {e}", m.name)))?;
        // **One `/props` probe decides the whole switch** — weights, dialect,
        // marker — before anything moves, so a refusal leaves the session exactly
        // as it was. The daemon's own vocabulary is optional since main made it
        // so (a box with only an API key starts on the byte one), and the
        // decision takes it as an `Option`: with one, behaviour is as it was;
        // without one, nothing can be the SAME weights as a vocabulary that does
        // not exist, and the probe alone settles which weights the switch seats
        // — the target's own, reported by the target.
        let switch = local_switch_decision(&want, m, self.cfg.vocab_gguf.as_deref())
            .map_err(HarnessError::Setup)?;

        // Off any metered provider first, and by its own door, so the ledger scale and
        // the restored window are handled where that is understood. Its report is
        // discarded deliberately: it describes a move to the daemon's own server, which
        // is a waypoint here rather than where this switch lands.
        //
        // **But not its re-seat half, when this switch replaces the engine anyway.**
        // `/models local`'s door returns a session sitting on the daemon's own
        // weights — re-rendered back, if a previous switch moved it — and a
        // conversation forked back only to be forked again onto the target's
        // dialect a moment later pays two cold prefills for one decision. So the
        // same-weights arm takes the full door and the other-weights arm takes
        // the door with its re-seat withheld: the engine this arm is about to
        // discard is not rebuilt first.
        match switch {
            LocalSwitch::SameWeights => {
                // **The UNGATED door, and the one place that matters.** This is a waypoint on the
                // road to a fleet model, not a switch the operator asked for: the gated door would
                // see a prompt in flight and record `local` as a pending switch, and the endpoint
                // would then move under it — half a switch, which is worse than either. A fleet
                // switch cannot arrive inside a turn anyway (`slash::mid_turn_switch_ok` refuses
                // it), and this keeps the two facts from disagreeing if that ever changes.
                self.set_provider_now(None)?;
                self.move_local_endpoint(&want, m)
            }
            LocalSwitch::OtherWeights {
                props,
                dialect,
                gguf,
            } => {
                self.set_own_server(false)?;
                self.set_local_model_weights(&want, m, props, dialect, gguf)
            }
        }
    }

    /// The address move a same-weights switch is: the engine, the dialect and the
    /// conversation all stay, and only where turns are posted changes.
    ///
    /// Split from [`Harness::set_local_model`] so the arm that changes nothing but
    /// the address is readable as exactly that, beside the arm that changes
    /// everything.
    fn move_local_endpoint(
        &mut self,
        want: &Endpoint,
        m: &letibot_provider::keys::LocalModel,
    ) -> Result<String, HarnessError> {
        // **The engine is what actually holds the address** — `cfg.endpoint` is the
        // record and `engine.endpoint` is where `/completion` is posted. Writing one
        // and not the other is how a switch reports a move it did not make.
        self.cfg.endpoint = want.clone();
        self.engine.endpoint = want.clone();
        self.cfg.model = m.model.clone();
        self.engine.model = m.model.clone();
        if !m.profile.sampling.is_empty() {
            let sampling = serde_json::Value::Object(m.profile.sampling.clone());
            self.cfg.sampling = sampling.clone();
            self.engine.sampling = sampling;
        }
        // A different box is a different KV cache and a different window; the ratio
        // measured on the old one is not evidence about this one.
        self.cfg.ledger_scale = None;
        // And a ratio measured on a CLOUD model -- `None` here unless this session came
        // from one -- is not evidence about a local ledger, whose count IS the prompt.
        self.cfg.carried_scale = None;
        let moved = self.retune_window_local(want, &m.name, m.profile.window);
        let line = format!(
            "turns go to `{}` at {} from the next one on (fleet, no meter){}{moved}",
            m.model,
            want.authority(),
            if m.profile.sampling.is_empty() {
                String::new()
            } else {
                format!(", sampling {}", render_sampling(&m.profile.sampling))
            }
        );
        Ok(line)
    }

    /// **The cross-weights switch** — the arm that makes `/models local.glm` work
    /// from a daemon started on the qwen weights.
    ///
    /// Everything here happens in an order where a failure leaves a session that
    /// still works: the probe and the dialect were decided before this was called,
    /// the vocabulary loads before anything is written, the fork is written before
    /// anything is swapped, and the engine moves only once the fork exists. A
    /// failure at any step is the sentence naming the step.
    fn set_local_model_weights(
        &mut self,
        want: &Endpoint,
        m: &letibot_provider::keys::LocalModel,
        props: letibot_turn::serving::ServedProps,
        dialect: crate::dialect::Dialect,
        gguf: std::path::PathBuf,
    ) -> Result<String, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(format!(
                "switching to the other weights at {} re-renders this conversation \
                 into a fork, and a fork needs a store to write it to; this session \
                 has none, so the weights, the dialect and the address were all left \
                 alone",
                want.authority()
            )));
        }
        // The GGUF `/props` named is a path on the box that RUNS the server; a
        // LAN target can name one this box cannot read, and the switch tokenizes
        // HERE. Refused with the path and what to do about it, rather than
        // searched for by basename — a basename hunt that found the wrong file
        // would be this switch's own corruption, committed by its own fix.
        if !gguf.is_file() {
            return Err(HarnessError::Setup(format!(
                "{} serves weights at `{}` (its /props answer), and that is not a file \
                 this box can read. A switch tokenizes here and sends ids, so it needs \
                 those weights' GGUF on THIS box — mount or link it at that path (the \
                 first shard of a split model is enough) and switch again. Nothing was \
                 changed.",
                want.authority(),
                gguf.display()
            )));
        }
        let old_dialect = self.cfg.dialect;
        // What tokenizes NOW, read before the engine below replaces it: a GGUF's
        // path, or `bytes:<hash>` for a daemon that started with no GGUF of its
        // own — which is exactly the daemon this arm is new for.
        let old_gguf = self.engine.vocab().source().to_string();
        let was_tokens = self.session.ledger.len();

        // **The vocabulary of the weights that will answer.** The second
        // vocabulary a process ever loads — the daemon's own came first, at
        // start — and the one a box short of memory cannot afford. It is CPU-only
        // by the shim's own construction (see `csrc/shim.c`), which is what lets
        // a daemon already holding one model's weights load another's tokenizer.
        let vocab = Arc::new(load_gguf(&gguf).map_err(|e| {
            HarnessError::Setup(format!(
                "loading the vocabulary at `{}`, which {}'s /props named: {e}. A \
                 switch tokenizes here and sends ids, so it needs that file on this \
                 box. Nothing was changed.",
                gguf.display(),
                want.authority()
            ))
        })?);
        let wiring = Arc::new(dialect.wiring(self.cfg.effort.as_deref()));
        let sampling = if m.profile.sampling.is_empty() {
            self.cfg.sampling.clone()
        } else {
            serde_json::Value::Object(m.profile.sampling.clone())
        };
        // Resolving the dialect's control and stop tokens against the target's
        // vocabulary is the fit check `open` runs at daemon start; refused here
        // for the same reason — at runtime a mismatch is silent.
        let mut engine = TurnEngine::new(
            vocab.clone(),
            wiring.renderer.clone(),
            wiring.parser.clone(),
            want.clone(),
            letibot_backend::BackendCaps::OWN_SERVER,
            m.model.clone(),
            sampling.clone(),
        )
        .map_err(|e| {
            HarnessError::Setup(format!(
                "the {} dialect does not fit the vocabulary at `{}`: {e}. Every \
                 control token and stop literal must resolve to exactly one vocab \
                 entry — a mismatch is a dialect/weights pairing error, refused now \
                 because at runtime it is silent. Nothing was changed.",
                dialect.name(),
                gguf.display()
            ))
        })?;
        engine.media_marker = props.media_marker.clone();

        // **The fork's prefix, keyed by the NEW dialect's own template sha.**
        // `stable_prefix.dialect_sha` is the key a resume checks before reusing a
        // rendering, so the row this writes is what makes a switch to another
        // dialect never reuse this one's cache — including by a daemon that
        // resumes this session later.
        let next = StablePrefix {
            system: self.cfg.system.clone(),
            tools_json: wiring.tools_json(&self.runtime.registry.schemas()),
        };
        let measured = engine
            .open(&format!("{}#switch-probe", self.cfg.session_id), &next)
            .map_err(|e| HarnessError::Setup(format!("rendering the new prompt: {e}")))?;
        let rec = StablePrefixRecord {
            dialect_sha: hex32(&wiring.spec().template_sha),
            system: next.system.clone(),
            tools_json: next.tools_json.clone(),
            tokens: measured.ledger.prefix_tokens().to_vec(),
            h_init: measured.ledger.h_init(),
            vocab_source: gguf.display().to_string(),
        };
        drop(measured);
        let next_id = self
            .store
            .as_ref()
            .expect("refused at the top of this method")
            .put_stable_prefix(&rec)
            .map_err(|e| HarnessError::Store(e.to_string()))?;

        // **The engine moves first**, because the fork below renders with
        // `self.engine` — and the fork is the first thing that can no longer fail
        // cheaply, so by the time it runs, everything it renders with is already
        // the pair the next turn will use.
        self.engine = engine;
        // **And the counter the notes pane measures with moves with it.** The section is
        // assembled with the engine's own vocabulary, so a pane that went on measuring with
        // the one this session OPENED on would disagree with the prompt about which notes are
        // whole — the one thing the pane exists to show. See `Harness::install_notes_source`,
        // whose closure reads this field.
        self.note_vocab = vocab.clone();
        self.render = wiring.clone();
        self.cfg.dialect = dialect;
        self.cfg.vocab_gguf = Some(gguf.clone());
        self.cfg.endpoint = want.clone();
        self.cfg.model = m.model.clone();
        self.cfg.sampling = sampling;
        self.cfg.media_marker = props.media_marker.clone();
        self.cfg.ledger_scale = None;
        // The new weights count their own tokens: an estimate carried from a cloud model
        // would be a conversion of a local count into something it is not.
        self.cfg.carried_scale = None;

        // **The conversation, re-rendered whole under the new pair.** The items
        // are dialect-neutral (`item_json`); the tokens were a cache of one
        // rendering — the same rationale the resume fork is built on, and the
        // same machinery `reingest` drives: fork, carry every item, summarise
        // nothing.
        let items = self.session.items.clone();
        let outcome = CompactionOutcome {
            turn_id: format!("{}#weights", self.transcript_id),
            summary: String::new(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
        };
        self.compacting = true;
        let forked = self.fork_to_summary(
            &outcome,
            Some(&next),
            Some(&next_id),
            ForkTail {
                items: &items,
                split: None,
                because: "",
            },
        );
        self.compacting = false;
        let fork = forked?;
        self.adopt_reseat(Some((next, next_id)));

        // A different box is a different KV cache and a different window; the
        // ratio measured on the old one is not evidence about this one, and the
        // conversation just changed size under it anyway.
        let moved = self.retune_window_local(want, &m.name, m.profile.window);
        let line = format!(
            "turns go to `{}` at {} from the next one on (fleet, no meter). Weights \
             changed — `{}` → `{}`, dialect {} → {} — so this conversation's {} \
             item(s) were re-rendered for them: {} → {} tokens, fork {}, one cold \
             prefill on the next turn{}{}",
            m.model,
            want.authority(),
            old_gguf,
            gguf.display(),
            old_dialect.name(),
            dialect.name(),
            items.len(),
            was_tokens,
            fork.base_tokens,
            fork.transcript_id,
            if m.profile.sampling.is_empty() {
                String::new()
            } else {
                format!(", sampling {}", render_sampling(&m.profile.sampling))
            },
            moved
        );
        self.publish_settings();
        Ok(line)
    }

    /// **Point `context_window` at the box that will answer** — the fleet half of
    /// [`Harness::retune_window`], reading the server instead of the catalogue.
    ///
    /// This is not symmetry for its own sake. Measured on this fleet the day the
    /// switch landed: `127.0.0.1:8080` serves the 27B at `n_ctx` 262144 and
    /// `192.168.1.78:8082` serves **the same GGUF** at 57344. Without this the
    /// switch inherited the window this daemon started with, so a session that moved
    /// to .78 planned compaction against 262144 tokens on a server holding 57344 —
    /// and the conversation would run off the end of the KV cache with every local
    /// check saying there was room. The weights matching is exactly what made it
    /// easy to miss: the vocabulary check passes, and the window is the thing that
    /// does not travel with it.
    ///
    /// A server that will not report `n_ctx` leaves the window `None`, which is this
    /// config's word for *no wall* rather than for a large one — the same reading
    /// `cli` takes for a local endpoint that will not say. It is said out loud,
    /// because planning against nothing is a decision the operator should see.
    fn retune_window_local(&mut self, want: &Endpoint, name: &str, stated: Option<u64>) -> String {
        // Remembered on the way out, so `/models local` restores the server this
        // daemon was started against rather than keeping another box's number. Same
        // write site and same mirror as `retune_window`, so the two cannot disagree.
        if self.local_window.is_none() {
            self.local_window = Some(self.cfg.context_window);
            if let Ok(mut g) = self.local_window_cell.lock() {
                *g = self.local_window;
            }
        }
        let was = self.cfg.context_window;
        // **A stated window beats the probe**, for the same reason `--context-window`
        // beats `/props` at startup: the operator naming a number is them telling the
        // daemon something it cannot work out, and the case the key exists for is a
        // proxy whose `/props` answers about itself rather than the server behind it.
        let now = stated.or_else(|| letibot_turn::serving::served_ctx(want));
        self.cfg.context_window = now;
        match now {
            Some(w) if Some(w) == was => String::new(),
            Some(w) => {
                let resident = self.session.ledger.len() as u64;
                let mut said = match was {
                    Some(old) => format!(". Context window {old} → {w}"),
                    None => format!(". Context window now {w}"),
                };
                // The consequence, not just the number — `retune_window`'s own rule:
                // being told after the turn that it had to compact is being told
                // too late. A move to a SMALLER box is where this bites, and that is
                // the ordinary direction for a fleet switch.
                if resident + self.cfg.headroom() >= w {
                    said.push_str(&format!(
                        "; this conversation is {resident} token(s), so the next turn \
                         compacts first"
                    ));
                }
                said
            }
            None => format!(
                ". {} did not report an n_ctx, so there is no wall to plan against — \
                 put a window on [model.\"{name}\"] or expect no compaction",
                want.authority()
            ),
        }
    }

    /// **Ask the operator for the key [`Harness::key_wanted`] names, and make it the
    /// session's provider.**
    ///
    /// The masked card is the sudo password's: [`letibot_sessionlog::hub::Hub::request_secret`]
    /// raises `SecretRequested` to every head and the answer comes back on a channel, never
    /// in an event — so the key reaches providers.toml and nowhere else. The worker blocks
    /// while it waits, which is safe for the same reason it is for sudo: the answer is
    /// handled on the answering head's own reader thread.
    ///
    /// Refuses at once, rather than after a wait, when no head that answers decisions is
    /// attached (a one-shot `--prompt`, say): the sentence then names the variable and the
    /// file instead. A key that providers.toml could not hold is asked for again, saying
    /// why, up to three times. `why` is the reason for asking again — a 401's own words.
    fn obtain_key(&mut self, why: Option<String>) -> Result<(), HarnessError> {
        let Some(pc) = self.key_wanted.clone() else {
            return Ok(());
        };
        let preset = letibot_provider::Preset::parse(&pc.name).map_err(HarnessError::Setup)?;
        let file = letibot_provider::keys::config_file();
        if self.hub.deciding_heads().is_empty() {
            return Err(HarnessError::Setup(format!(
                "no key for `{name}`, and no head is attached to ask for one. Set ${env}, or put \
                 `key = \"…\"` under [{name}] in {file}",
                name = pc.name,
                env = preset.key_env,
                file = file.display(),
            )));
        }
        let mut reason = why;
        for _ in 0..3 {
            // The prompt is persisted with the request (the secret never is), so it says
            // where the key goes and nothing it should not.
            let prompt = format!(
                "{name} needs an API key. It is saved to {file} (mode 0600, under [{name}]) \
                 and this session's turns go to {name}.{again}",
                name = pc.name,
                file = file.display(),
                again = reason
                    .as_deref()
                    .map(|r| format!(" Asking again: {r}"))
                    .unwrap_or_default(),
            );
            let deadline = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| (d + KEY_PATIENCE).as_millis() as u64)
                .unwrap_or(0);
            // An empty `command` is what tells a head this is not sudo's card.
            let (req_id, rx) = self.hub.request_secret(&prompt, "", deadline);
            let given = match rx.recv_timeout(KEY_PATIENCE) {
                Ok(s) => s,
                Err(_) => {
                    self.hub
                        .abandon_secret(&req_id, "nobody, before the deadline");
                    None
                }
            };
            let Some(key) = given.map(|k| k.trim().to_string()) else {
                return Err(HarnessError::Setup(format!(
                    "no key was given for `{name}`, so this turn did not run and nothing was \
                     sent. Send the message again to be asked again, or set ${env}, or put \
                     `key = \"…\"` under [{name}] in {file}",
                    name = pc.name,
                    env = preset.key_env,
                    file = file.display(),
                )));
            };
            if let Some(bad) = unsavable_key(&key) {
                reason = Some(format!("that key was not saved — {bad}."));
                continue;
            }
            letibot_provider::keys::store_key(Some(&file), &pc.name, &key).map_err(|e| {
                HarnessError::Setup(format!("saving the key to {}: {e}", file.display()))
            })?;
            // **The first key on a box also chooses the default**, so the next `letibot`
            // starts here without a flag. Only when there is none: an existing choice is the
            // operator's and is not this card's to move.
            let mut made_default = false;
            if let Ok(None) = letibot_provider::keys::default_choice(Some(&file)) {
                made_default =
                    letibot_provider::keys::set_default(Some(&file), &pc.name, pc.model.as_deref())
                        .is_ok();
            }
            self.key_wanted = None;
            self.set_provider(Some(pc.clone()))?;
            self.hub.publish(SessionEvent::Warning {
                code: "provider_key_saved".into(),
                detail: format!(
                    "the key for {name} is saved in {file}{default}; this session's turns go to \
                     {name}",
                    name = pc.name,
                    file = file.display(),
                    default = if made_default {
                        format!(", and [default] provider = \"{}\" is set", pc.name)
                    } else {
                        String::new()
                    },
                ),
                compaction: None,
            });
            return Ok(());
        }
        Err(HarnessError::Setup(format!(
            "no usable key for `{}` after three tries: {}",
            pc.name,
            reason.unwrap_or_default()
        )))
    }

    /// **Switch what answers this session's turns**, underneath the conversation.
    /// `None` is the local server. The transcript, the ledger and the tools are
    /// untouched: the next turn simply goes elsewhere. Returns a line saying what
    /// answers now; a provider whose key cannot be found is refused and nothing
    /// changes.
    ///
    /// # A switch never lands inside a turn
    ///
    /// **While a prompt is in flight the request is PENDING**, and this returns the
    /// sentence that says when it will take: it is recorded ([`Harness::pending_switch`]),
    /// said on the hub (`model_switch_pending`), and applied where the prompt span closes
    /// ([`Harness::apply_pending_switch`], from [`Harness::end_turn_clock`]).
    ///
    /// The measurement is the whole of the rule. MEASURED 2026-10-26, the operator's
    /// session: a `/models deepseek/deepseek-flash` typed while a round on `glm-coding` was
    /// failing was applied inside the retry loop, so the round was re-issued on deepseek
    /// carrying the rest of GLM's turn — a run of assistant tool-call rounds with no
    /// `reasoning_content`, which deepseek's thinking mode refuses
    /// (`http 400: The reasoning_content in the thinking mode must be passed back to the
    /// API`). A model's first request must follow a USER message, so the turn that was
    /// running on the old model has to end before the new one speaks. Their words for the
    /// rule: *"we can switch after turn ends"*.
    ///
    /// **Two things are not a switch and are not deferred.** A provider built for the
    /// model the session is ALREADY on is a rebuild (`Harness::obtain_key` renews a
    /// refused key through this very door, and a deferred rebuild would retry the round
    /// with the key the provider just refused), so the target name is compared against
    /// [`Harness::model_name`] first and an unchanged name goes straight through.
    pub fn set_provider(
        &mut self,
        choice: Option<crate::config::ProviderConfig>,
    ) -> Result<String, HarnessError> {
        if self.prompt_in_flight.load(Ordering::SeqCst) {
            let target = model_name_of(&choice);
            if target != self.model_name() {
                return Ok(self.pend_switch(choice, target));
            }
        }
        self.set_provider_now(choice)
    }

    /// **Record a switch for the end of this turn, and say so** — the pending half of
    /// [`Harness::set_provider`].
    ///
    /// The sentence is the return value (so the verb's reply carries it) *and* a
    /// `model_switch_pending` warning (so a head that is not the one that typed it — and the
    /// one that is, once the reply has scrolled — has it on the status line rather than only in
    /// the scrollback).
    ///
    /// **It does not build the provider.** A switch that would be refused — a key that does not
    /// resolve — is refused when it is applied, and `model_switch_not_applied` is what says so
    /// then. Building it here would either refuse a request the turn cannot act on yet or build
    /// one twice.
    fn pend_switch(
        &mut self,
        choice: Option<crate::config::ProviderConfig>,
        target: String,
    ) -> String {
        let now = self.model_name();
        let said = format!("turns go to {target} when this turn ends; this one finishes on {now}");
        self.pending_switch = Some(PendingSwitch {
            choice,
            model: target,
        });
        self.hub.publish(SessionEvent::Warning {
            code: "model_switch_pending".into(),
            detail: said.clone(),
            compaction: None,
        });
        said
    }

    /// **The switch itself**, with no question about the turn span — [`Harness::set_provider`]
    /// is the door everything asks through, and this is what it calls once nothing is in flight.
    ///
    /// `end_turn_clock` is the one caller that must not go through the gate (the span has just
    /// closed, and it is applying what the gate recorded).
    fn set_provider_now(
        &mut self,
        choice: Option<crate::config::ProviderConfig>,
    ) -> Result<String, HarnessError> {
        // **A switch clears the ratio, in BOTH directions** — ruled 2026-09-24, and the
        // reason is this document's own: **a measurement made on one model is not evidence
        // about another.** `ledger_scale` is `(ledger tokens, the model's prompt tokens)` for
        // one prompt on one model, and carrying it across a switch presents a stale
        // measurement as a current one.
        //
        // **Not re-derived.** There is nothing to re-derive it FROM at the moment of the
        // switch — the first round on the new model is what produces it — so "re-derive"
        // would mean inventing a ratio, which is a guess wearing a number's clothes. Cleared,
        // and the first round sets it; until then the unscaled count stands, which
        // `Config::planning_window` already returns when the scale is `None`, and
        // `Config::tokens_are_unscaled` is how a reader says so rather than treating it as
        // calibrated.
        //
        // **The old pair is KEPT, as an estimate that names the model it was measured
        // on** — the half of this that used to be wrong. Clearing is right: the first
        // round on the new model's tokenizer is what measures it, and until then nothing
        // about this model has been measured. What clearing must not do is turn the
        // ledger's own bytes into the number compared with the new model's window:
        // MEASURED 2026-10-25, a session switched to `glm-coding` stopped at round 0 with
        // `2708271 of 1000000 ledger tokens (unscaled: no round on glm-coding has measured
        // yet)` and never sent a byte. See `Config::CarriedScale` for the whole of it, and
        // `Config::carried_note` for how every line that prints a converted count says
        // whose measurement it is.
        //
        // **The clear is inside each arm and not before the `match`**, because this method
        // promises that a switch which cannot be built changes nothing — and a scale dropped
        // for a provider whose key turned out to be missing would be a change made by a
        // refusal. `carry_the_scale` is called after the provider that can fail is built,
        // on the same line as the clear it is the other half of.
        match choice {
            // The daemon's own server takes token ids, and a byte-vocabulary session has
            // none it could read: refused before anything changes, as the promise above
            // says. (main's guard, kept: a daemon with no vocabulary of its own has
            // nothing `/models local` could return to. The guard and the return below
            // read the same fact — the vocabulary the engine tokenizes with NOW, which
            // a cross-weights switch may have replaced since the daemon started.)
            None if self.engine.vocab().is_bytes() => {
                Err(HarnessError::Setup(NO_VOCAB_FOR_LOCAL.to_string()))
            }
            // **`/models local` is a RETURN** — to the daemon's own weights, address,
            // sampling, marker, window and vocabulary, re-seated — and not a provider
            // clear. `set_own_server` is this arm; see its doc for the re-seat half.
            None => self.set_own_server(true),
            Some(pc) => {
                let p = build_provider(&pc, &self.cfg.sampling, provider_notes(&self.hub))
                    .map_err(HarnessError::Setup)?;
                let mut line = format!(
                    "turns go to {}/{} from the next one on — METERED; the prefix check is \
                     skipped, the ledger stays the record",
                    p.name(),
                    p.model()
                );
                // **The window follows the model.** Compaction is planned against
                // `context_window` — `plan_overrun`, `should_compact` and
                // `headroom` all read it — and it came from the LOCAL server's
                // `/props`. Leaving it there meant a conversation answered by a
                // cloud model was measured against the llama-server's window, and
                // compacted at the wrong time or not at all.
                line.push_str(&self.retune_window(&pc));
                // After everything that can fail, and before anything reads it.
                self.note_the_way_back();
                self.carry_the_scale();
                self.cfg.ledger_scale = None;
                self.provider = Some(p);
                self.cfg.provider = Some(pc);
                // **Say it, or the head goes on drawing the old name.** The
                // header reads the model from the daemon's word, and the daemon's
                // word was sent once at attach: the operator switched leticl to
                // deepseek and the top row still said qwen, indefinitely. The
                // `model` settings row carries what answers now, and this is what
                // makes it carry it.
                self.publish_settings();
                Ok(line)
            }
        }
    }

    /// **Keep the ratio a switch is about to clear, named after the model that measured
    /// it** — the other half of the clear in [`Harness::set_provider`], and see
    /// `Config::CarriedScale` for why the pair is kept at all.
    ///
    /// Called only where the switch SUCCEEDED: a switch refused for a missing key changes
    /// nothing, and a scale moved by a refusal would be a change made by nothing.
    ///
    /// **A second switch with nothing measured in between keeps the older pair rather
    /// than overwriting it with `None`** — `deepseek → glm-coding → grok` is one session
    /// whose only measurement was deepseek's, and that is still the best estimate there is
    /// for either of the others.
    fn carry_the_scale(&mut self) {
        let Some((ledger, provider)) = self.cfg.ledger_scale else {
            return;
        };
        // The model being LEFT, in `/models`' spelling (`deepseek/deepseek-flash`), read
        // before the caller overwrites anything.
        let model = self.model_name();
        self.cfg.carried_scale = Some(crate::config::CarriedScale {
            model,
            ledger,
            provider,
        });
    }

    /// **Remember the model this switch is leaving, so a refusal can name the way back.**
    ///
    /// Read BEFORE anything moves, and only where the move cannot fail after it — see
    /// [`Harness::switch_back_notice`] for what reads it.
    fn note_the_way_back(&mut self) {
        self.switch_back = Some(SwitchBack {
            previous: self.model_name(),
        });
    }

    /// **A switch that fails says how to go back** — the notice a provider's refusal earns
    /// on the first round after a model switch, or `None` when this is not that.
    ///
    /// MEASURED 2026-10-25, `s-1791394076177451000`: a long session was switched to
    /// `glm-coding`, and every request it sent came back
    /// `http 400: messages.content.type is invalid, allowed values: ['text']`. The screen
    /// said `FAILED — http 400: …` and stopped there; the operator had to remember what
    /// the session had been on and type it back by hand, and the daemon knew both the name
    /// and the command. That is the whole of this function.
    ///
    /// **4xx only**, because that is the class that refuses *what was sent*: a 429 is the
    /// ladder's (it waits, and may fall back), a 5xx is an endpoint that is unwell, and
    /// neither is evidence about the model. A transport failure says nothing about it
    /// either.
    ///
    /// The armed state is TAKEN, so one switch earns one notice: a second failure is a
    /// session that has a problem the way back does not fix, and repeating the advice
    /// every turn is how a warning becomes furniture.
    pub fn switch_back_notice(&mut self, e: &HarnessError) -> Option<String> {
        let back = self.switch_back.take()?;
        let code = provider_refusal(e)?;
        let now = self.model_name();
        if now == back.previous {
            // The round went to the model this session already was on — a switch that
            // landed on the name in force is not a switch, and there is nowhere back.
            return None;
        }
        Some(switch_back_said(&back.previous, &now, code))
    }

    /// **The whole of `/models NAME`, in one place, for both callers.**
    ///
    /// There are two: the worker's arm (`Sessions::slash`, between turns, which is where the
    /// verb has always been answered) and the retry loop's poll of the queue
    /// ([`Harness::take_queued_model`], which is the operator's own line answered DURING the turn
    /// they typed it in rather than after it). Two copies of a switch would be two places for the
    /// screen, the session row and the thing that actually answers to disagree — which is the
    /// defect [`Harness::persist_provider_choice`] was written for, one door along.
    ///
    /// **A switch asked for while a prompt is in flight is recorded and NOT applied** — see
    /// [`Harness::set_provider`]. The reply still comes back `ok`, and it says when the switch
    /// takes (*"turns go to X when this turn ends"*), which is the honest answer: it was accepted,
    /// and it is not in force yet.
    ///
    /// **The choice is resolved by the caller, not here.** `models_choice` needs no
    /// harness, and the worker's path must still carry the resolver's own notes when
    /// the session it names is not open — so what this takes is the resolved choice
    /// and the lines it came with.
    pub fn apply_model_choice(
        &mut self,
        choice: crate::slash::ModelChoice,
        mut lines: Vec<String>,
    ) -> crate::slash::SlashReply {
        // **What answers before, so the row is written only when it moved.** A pending switch
        // leaves this name alone, and a session row written with the OTHER model's name would
        // make a restart resume on a model this daemon never ran.
        let was = self.model_name();
        let applied = match choice {
            crate::slash::ModelChoice::OwnServer => self.set_provider(None),
            crate::slash::ModelChoice::Metered(pc) => self.set_provider(Some(pc)),
            // Its own door, because it verifies the vocabulary before it moves
            // anything — see `Harness::set_local_model`.
            crate::slash::ModelChoice::Local(m) => self.set_local_model(&m),
        };
        match applied {
            Ok(line) => {
                // **The choice goes to the session row in the same breath** —
                // `persist_provider_choice`'s own doc is why. A switch that reached
                // the screen but not the row was a choice that expired with the
                // process, which is the defect this whole change is. A pending one is written
                // where it is applied instead ([`Harness::apply_pending_switch`]).
                if self.model_name() != was
                    && let Err(why) = self.persist_provider_choice()
                {
                    lines.push(why);
                }
                lines.push(line);
                crate::slash::SlashReply { lines, ok: true }
            }
            Err(e) => {
                lines.push(e.to_string());
                crate::slash::SlashReply { lines, ok: false }
            }
        }
    }

    /// **Back to the daemon's own server** — the `None` arm of [`Harness::set_provider`].
    ///
    /// `reseat_foreign` is the one honest wrinkle: `/models local` means *this
    /// daemon's own server, exactly as it was started*, so a session sitting on
    /// another fleet model's WEIGHTS comes back to the daemon's vocabulary and
    /// dialect here, conversation re-rendered into a fork under them. But a switch
    /// straight from one fleet model's weights to another's calls this only to get
    /// off a metered provider first — its engine is about to be replaced, and a
    /// conversation forked back to the daemon's dialect only to be forked again a
    /// moment later pays two cold prefills for one decision — so that caller passes
    /// `false` and takes the field clears without the re-seat.
    fn set_own_server(&mut self, reseat_foreign: bool) -> Result<String, HarnessError> {
        self.note_the_way_back();
        self.provider = None;
        self.cfg.ledger_scale = None;
        // **And the carried pair goes with it**: coming home means the ledger IS the
        // prompt again (the engine sends its own token ids), so a ratio measured by a
        // cloud model would convert a count that needs no conversion. See
        // `Config::scale`, which would ignore it anyway — cleared here so that the only
        // state a reader has to reason about is the one that applies.
        self.cfg.carried_scale = None;
        self.cfg.provider = None;
        // **The return itself**: address, alias, sampling, marker, vocabulary,
        // dialect — every field a fleet switch overwrote, given back the value
        // the daemon started with. The window is restored from its own memory
        // below, the same way it always was.
        //
        // Before this was a return it was half of one: the address stayed
        // wherever the last fleet switch put it while the line still said "the
        // local server", and the operator's own file documents `local` as
        // *(the bare word) is the daemon's own model*. A line that names the
        // wrong box is the provenance defect in report form.
        let own = self.own_server.clone();
        let mut reseat = String::new();
        let foreign = self.on_foreign_weights();
        if foreign && reseat_foreign {
            // The mirror of the switch's cross-weights arm, aimed home: the
            // daemon's own pair, the conversation re-rendered under it, a fork
            // keyed by the daemon's own template sha. It can only fail where the
            // switch can fail, and for the same reasons. What moved is said in
            // the values AS THEY STOOD — the re-seat overwrites them, so they are
            // read before it runs.
            // The names in the line are the vocabularies' own `source()`s — a
            // GGUF's path, or `bytes:<hash>` for a daemon that started on the
            // byte vocabulary — read before the re-seat overwrites them.
            let (was_vocab, was_dialect) =
                (self.engine.vocab().source().to_string(), self.cfg.dialect);
            self.reseat_to_own(&own)?;
            reseat = format!(
                ". Weights back — `{}` → `{}`, dialect {} → {} — and the conversation \
                 was re-rendered for them again: one more cold prefill, one more fork",
                was_vocab,
                own.vocab.source(),
                was_dialect.name(),
                own.dialect.name()
            );
        } else if foreign {
            // Off a metered provider on the way to another fleet model's weights:
            // everything the cross-weights switch is about to overwrite is left
            // exactly as it is, and the fork that would waste a prefill is not run.
        } else {
            self.cfg.endpoint = own.endpoint.clone();
            self.cfg.model = own.model.clone();
            self.engine.endpoint = own.endpoint.clone();
            self.engine.model = own.model.clone();
            if self.cfg.media_marker != own.media_marker {
                self.cfg.media_marker = own.media_marker.clone();
                self.engine.media_marker = own.media_marker;
            }
            // A fleet switch may have replaced the sampling; the daemon's own
            // server gets the sampling it started with back.
            self.cfg.sampling = own.sampling.clone();
            self.engine.sampling = own.sampling;
        }
        // Back to the server's own window, which is the one its `/props`
        // reported at startup. Restored rather than recomputed: the local
        // endpoint is not asked again here, and keeping a cloud model's
        // window over a local conversation is the same bug pointing the
        // other way.
        let mut line = format!(
            "turns go to the local server at {} ({}) from the next one on",
            self.cfg.endpoint.authority(),
            self.cfg.model
        );
        if let Some(w) = self.local_window.take() {
            if w != self.cfg.context_window {
                line.push_str(&match w {
                    Some(w) => format!(". Context window back to {w}"),
                    None => ". The local server never reported a window".into(),
                });
            }
            self.cfg.context_window = w;
        }
        line.push_str(&reseat);
        self.publish_settings();
        Ok(line)
    }

    /// **Is this session on weights that are not the daemon's own?**
    ///
    /// The one question that decides whether a return to the daemon's own server is an
    /// address move or a re-seat: [`Harness::set_own_server`] re-renders the conversation
    /// into a fork when the vocabulary or the dialect has been moved by a fleet switch,
    /// and only then.
    ///
    /// Named rather than inlined because it now has two readers, and they are asking
    /// different questions of the same fact: `set_own_server` is deciding whether to
    /// fork, and `slash::mid_turn_switch_ok` is deciding whether a RUNNING TURN may
    /// carry a `/models local` out itself — which it may exactly when the return is the
    /// cheap arm. Two copies of this expression would be two answers to that.
    fn on_foreign_weights(&self) -> bool {
        self.cfg.vocab_gguf != self.own_server.vocab_gguf
            || self.cfg.dialect != self.own_server.dialect
    }

    /// The re-seat half of a return to the daemon's own weights: the mirror of
    /// [`Harness::set_local_model_weights`], aimed at the [`OwnServer`] pair.
    ///
    /// Everything is the same shape and for the same reasons — the engine is
    /// rebuilt from the daemon's own vocabulary and wiring (no load, they are the
    /// `Arc`s open captured), the fork's prefix row is keyed by the daemon's own
    /// template sha, the conversation is re-rendered whole and nothing is
    /// summarised. The one thing that differs is where the facts come from: not a
    /// probe, the record.
    fn reseat_to_own(&mut self, own: &OwnServer) -> Result<(), HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "coming back to this daemon's own weights re-renders the conversation \
                 into a fork, and a fork needs a store to write it to; this session \
                 has none, so it stays on the weights it is on"
                    .into(),
            ));
        }
        let mut engine = TurnEngine::new(
            own.vocab.clone(),
            own.wiring.renderer.clone(),
            own.wiring.parser.clone(),
            own.endpoint.clone(),
            letibot_backend::BackendCaps::OWN_SERVER,
            own.model.clone(),
            own.sampling.clone(),
        )
        .map_err(|e| {
            HarnessError::Setup(format!(
                "the {} dialect does not fit the vocabulary at `{}`: {e} — the pair \
                 this daemon STARTED with, which cannot have drifted. This is a bug, \
                 not a configuration: the same pair opened this session.",
                own.dialect.name(),
                own.vocab.source()
            ))
        })?;
        engine.media_marker = own.media_marker.clone();
        let next = StablePrefix {
            system: self.cfg.system.clone(),
            tools_json: own.wiring.tools_json(&self.runtime.registry.schemas()),
        };
        let measured = engine
            .open(&format!("{}#return-probe", self.cfg.session_id), &next)
            .map_err(|e| HarnessError::Setup(format!("rendering the home prompt: {e}")))?;
        let rec = StablePrefixRecord {
            dialect_sha: hex32(&own.wiring.spec().template_sha),
            system: next.system.clone(),
            tools_json: next.tools_json.clone(),
            tokens: measured.ledger.prefix_tokens().to_vec(),
            h_init: measured.ledger.h_init(),
            vocab_source: own.vocab.source().to_string(),
        };
        drop(measured);
        let next_id = self
            .store
            .as_ref()
            .expect("refused above")
            .put_stable_prefix(&rec)
            .map_err(|e| HarnessError::Store(e.to_string()))?;

        self.engine = engine;
        // The same assignment as the switch's, in the other direction: the session is back on
        // the vocabulary it opened on and the notes source measures with what the section
        // will. See `Harness::install_notes_source`.
        self.note_vocab = own.vocab.clone();
        self.render = own.wiring.clone();
        self.cfg.dialect = own.dialect;
        self.cfg.vocab_gguf = own.vocab_gguf.clone();
        self.cfg.endpoint = own.endpoint.clone();
        self.cfg.model = own.model.clone();
        self.cfg.sampling = own.sampling.clone();
        self.cfg.media_marker = own.media_marker.clone();

        let items = self.session.items.clone();
        let outcome = CompactionOutcome {
            turn_id: format!("{}#home", self.transcript_id),
            summary: String::new(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
        };
        self.compacting = true;
        let forked = self.fork_to_summary(
            &outcome,
            Some(&next),
            Some(&next_id),
            ForkTail {
                items: &items,
                split: None,
                because: "",
            },
        );
        self.compacting = false;
        // The engine moved first, so a fork failure here would leave the session
        // rendering the own dialect over a foreign-rendered ledger — the
        // corruption this whole file refuses. The fork cannot fail halfway
        // (`fork_to_summary` writes every store row before it swaps), so this
        // names the state rather than papering over it.
        forked.map_err(|e| {
            HarnessError::Setup(format!(
                "re-rendering the conversation for this daemon's own weights: {e}. \
                 The switch to them did not complete; stay on the weights the \
                 conversation is rendered for and re-try the switch"
            ))
        })?;
        self.adopt_reseat(Some((next, next_id)));
        Ok(())
    }

    /// Point `context_window` at the model that will answer, and say what moved.
    ///
    /// Returns the sentence to append to the switch's report, empty when nothing
    /// changed. An unknown model leaves the window alone **and says so** — the
    /// silent half of this bug was that nobody could tell which window was in
    /// force.
    fn retune_window(&mut self, pc: &crate::config::ProviderConfig) -> String {
        // Remembered on the way out to the first provider, so `/models local`
        // restores the server's own number rather than keeping a cloud model's.
        if self.local_window.is_none() {
            self.local_window = Some(self.cfg.context_window);
            // **And mirrored to the runner**, which needs it for a `local` child spawned
            // while THIS session is on a provider. One write site, so the two cannot
            // remember different windows.
            if let Ok(mut g) = self.local_window_cell.lock() {
                *g = self.local_window;
            }
        }
        let Ok(preset) = letibot_provider::Preset::parse(&pc.name) else {
            return String::new();
        };
        let cat = letibot_provider::catalogue::Catalogue::load();
        let was = self.cfg.context_window;
        match preset.window(pc.model.as_deref(), &cat) {
            Some(w) if Some(w) == was => String::new(),
            Some(w) => {
                self.cfg.context_window = Some(w);
                let resident = self.session.ledger.len() as u64;
                let mut said = match was {
                    Some(old) => format!(". Context window {old} → {w}"),
                    None => format!(". Context window now {w}"),
                };
                // The consequence, not just the number: a switch that puts the
                // conversation over the new model's wall compacts on the next
                // turn, and being told afterwards is being told too late.
                if resident + self.cfg.headroom() >= w {
                    said.push_str(&format!(
                        "; this conversation is {resident} token(s), so the next turn \
                         compacts first"
                    ));
                }
                said
            }
            None => format!(
                ". The catalogue has no window for {}/{}, so compaction still plans \
                 against {} — check that is the right size before a long turn",
                pc.name,
                pc.model
                    .clone()
                    .unwrap_or_else(|| preset.default_model(&cat)),
                match was {
                    Some(w) => w.to_string(),
                    None => "no window at all".into(),
                }
            ),
        }
    }

    /// The jobs this session has started, one line each, for `/job` with no
    /// argument. The pane draws the same facts; this is for reading them without
    /// leaving the composer, and for finding an id to pass to `/job ID`.
    /// **One row, one line.** A command's text is whatever the model wrote, and a
    /// heredoc carries newlines inside it: `cd … && python3 - <<'PY'\ns = open("…`.
    /// Truncating that to 60 *characters* keeps the newline, so a row meant to be one
    /// line became two, and a listing of many of them became unreadable — the second
    /// half of what the operator saw on 2026-09-20.
    ///
    /// Every run of whitespace becomes one space before the cut, so the width the
    /// caller asks for is the width it gets. The ellipsis is added only when
    /// something was actually removed, so a short command is not decorated with a
    /// promise of more.
    fn one_line(command: &str, width: usize) -> String {
        let flat = command.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() <= width {
            return flat;
        }
        let cut: String = flat.chars().take(width).collect();
        format!("{cut}…")
    }

    /// **Which of the host's jobs a person would act on.**
    ///
    /// Every command runs in a job scope, so `ProcessHost::jobs()` is the whole
    /// session's process history. The two kinds worth a row are the ones
    /// somebody can still do something about: a job that was put in the
    /// background — `background` is `Some` exactly then, by its own doc — and
    /// anything still running, which is what ctrl-o moves and what an operator
    /// might want to interrupt, whether or not it was ever backgrounded.
    ///
    /// A finished foreground `grep` is neither, and three hundred of them are
    /// what `/job` printed before this existed.
    fn worth_listing(j: &letibot_tools::exec::JobView) -> bool {
        j.background.is_some() || j.state.is_running()
    }

    /// **The jobs, as data, for any head that asks.**
    ///
    /// The same list `job_lines` prints, before it is prose. Every decision in it
    /// is the daemon's: which jobs are worth listing (`worth_listing`), what the
    /// command reads as (`one_line`), what the state word is. A head renders
    /// these rows; it does not rebuild them out of the event stream, which is how
    /// a job that outlived its turn used to lose its name and how each head came
    /// to need its own copy of the rules. The operator, 2026-09-20: *"regarding
    /// jobs, subagents, etc, i expect them to be handled by harnessd not the
    /// heads"*.
    pub fn job_entries(&self) -> Vec<letibot_sessionlog::protocol::JobEntry> {
        let Some(host) = self.runtime.backend.processes() else {
            return Vec::new();
        };
        host.jobs()
            .iter()
            .filter(|j| Self::worth_listing(j))
            .map(Self::job_entry)
            .collect()
    }

    /// **One job as the wire carries it.** Pure, so the row can be asserted without a host:
    /// what this exists to pin is the name, and the honest emptiness of a job nobody named.
    fn job_entry(j: &letibot_tools::exec::JobView) -> letibot_sessionlog::protocol::JobEntry {
        letibot_sessionlog::protocol::JobEntry {
            id: j.id.0.clone(),
            command: Self::one_line(&j.command, 120),
            // **The name the caller stated, or nothing.** Never derived here either: this is
            // the daemon's listing, and a name it invented would be the same defect one
            // layer up from the tool.
            slug: j.slug.clone().unwrap_or_default(),
            how: j
                .background
                .as_ref()
                .map(|b| b.phrasing())
                .unwrap_or_default(),
            state: j.state.word(),
            running: j.state.is_running(),
            never_ran: j.state.never_ran(),
            // **Read out of the COMMAND, not out of what has been captured** (R41). Nothing
            // has to run to know it, and the answer does not change while the job does — so a
            // reader learns which of their running jobs cannot be watched before they open
            // the pane, rather than by finding an empty window and guessing why.
            redirect: letibot_tools::builtins::output_redirect_path(&j.command),
            produced: j.produced,
            elapsed_ms: j
                .ran_for
                .unwrap_or(j.elapsed)
                .as_millis()
                .min(u64::MAX as u128) as u64,
        }
    }

    /// **Install this session's live job-output read**, which is what a head's `ReadJobOutput`
    /// is answered from while the worker is busy with a turn
    /// (`letibot_sessionlog::registry::JobOutputSource`).
    ///
    /// It holds the process host and nothing else of the harness, so it reads exactly what
    /// [`Harness::job_output_window`] reads for a job the host holds — the same
    /// [`job_window_for`] decision over the same two reads. A job the host does not hold
    /// answers `None` and the read is queued, because that answer comes from the store.
    fn install_job_output_source(&self) {
        let Some(host) = self.runtime.backend.processes_arc() else {
            return;
        };
        self.session_registry.set_job_output_source(
            &self.cfg.session_id,
            Arc::new(move |job, offset| {
                live_job_output(host.as_ref(), job, offset, JOB_OUTPUT_WINDOW)
            }),
        );
    }

    /// **Install this session's standing-notes source**, which is what a head's `ListNotes`
    /// is answered from.
    ///
    /// A closure rather than a mailbox, and the vocabulary is the whole of why: the form each
    /// note has — verbatim, or an index because it did not fit what was left of the budget —
    /// is decided with [`Harness::note_vocab`], and the server thread that answers the frame holds
    /// no counter at all. So this hands the registry something that *can* measure
    /// (`letibot_sessionlog::registry::NotesSource`) and the read happens on the ask.
    ///
    /// **On the ask and not at a moment of our choosing**, because the notes are the
    /// operator's own files, edited in another window with nothing to tell this daemon: a
    /// mailbox refilled at session open would answer *what the corpus was when the section
    /// was last assembled*, which is a fact about the prompt, and a note written while the
    /// session sat idle would be missing from a pane that claims one row per note.
    ///
    /// Cheap enough to run per pane-open: a readdir, one read per note and one pass of the
    /// counter — the same work [`crate::standing_notes::section`] does at every base rebuild,
    /// and a person opens this pane rarely.
    fn install_notes_source(&self) {
        let workspace = self.cfg.workspace.clone();
        let global = crate::standing_notes::global_dir();
        // **The box's instruction directories, resolved once**: the pane re-reads the
        // corpus on every open and the answer cannot change while the daemon runs.
        let box_dirs = crate::leticode_config::BoxDirs::for_this_box();
        // **The vocabulary the section itself is measured with**, and a field of its own
        // rather than the engine's — see `Harness::note_vocab`, which tracks the engine
        // wherever it moves so the pane cannot measure with the vocabulary the session
        // *opened* on.
        let vocab = self.note_vocab.clone();
        self.session_registry.set_notes_source(
            &self.cfg.session_id,
            std::sync::Arc::new(move || {
                crate::standing_notes::rows(&workspace, &global, &box_dirs, &vocab)
            }),
        );
    }

    /// **Install this session's skills source**, which is what a head's `ListSkills` is answered
    /// from — the same rows the prompt's index is composed from, read on the ask.
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
                    })
                    .collect()
            }),
        );
    }

    /// **The operator hands this session one skill's body** — `/skills NAME`, and the `/skills`
    /// pane's Enter.
    ///
    /// Three rows' worth of decision in one place: the body is the operator's message (a `User`
    /// row with `Speaker::Operator`), the label says what it is and where it came from — a skill
    /// is not the operator's words, and a model that read it as an instruction would be following
    /// somebody else's prose on their word — and the rest of the line, when there is one, goes on
    /// as the operator's own prompt in the same turn.
    pub fn load_skill(&mut self, name: &str, then: &str) -> Result<Reply, HarnessError> {
        let skill = self.skills.load_any(name).map_err(HarnessError::Setup)?;
        self.submit(&skill_message(&skill, then))
    }

    /// **Say what this session is waiting on, if it has changed** — `PROTOCOL_VERSION` 52.
    ///
    /// Almost every move is caught by the hub itself: [`Hub::publish`] re-derives the value after an
    /// event whose fold can have moved it (`SessionEvent::touches_activity` — a turn starting or
    /// ending, a call, a card, a settlement, a child) and [`Hub::set_activity`] drops an unchanged
    /// one. So this is for the changes that happen with **no event at all**: a monitor firing on the
    /// waiter's own thread, a settlement queued by a watcher thread, a prompt sitting in the
    /// command queue. Called at the points the harness knows one of those to be true, and
    /// idempotent by construction: a value equal to the last one publishes nothing.
    ///
    /// **Why it is not a timer.** A tick would make the value eventually right, which is the shape
    /// of every heuristic this replaces. Each call site is a place where a fact changed.
    pub(crate) fn publish_activity(&self) -> bool {
        self.hub.refresh_activity()
    }

    /// Push the job table to the registry, where a head's `ListJobs` is answered
    /// from. Called wherever the table can have changed, for the same reason
    /// `publish_settings` is: a mailbox nobody refills is a stale answer that
    /// looks like a current one.
    pub fn publish_jobs(&self) {
        self.session_registry
            .set_jobs(&self.cfg.session_id, self.job_entries());
        // **And the monitors, on the same refill** — the monitors pane's rows, so a head that
        // asks after a round, a firing or a retire reads this moment's table.
        self.session_registry
            .set_monitors(&self.cfg.session_id, self.monitor_entries());
    }

    /// **This session's monitors as the wire carries them**: every live one, then the ended ones
    /// newest last ([`MONITORS_ENDED_SHOWN`] of them), each with how it ended. A continuous monitor
    /// is one row however often it fired, with the count.
    pub fn monitor_entries(&self) -> Vec<letibot_sessionlog::protocol::MonitorEntry> {
        let Some(monitors) = self.monitors.as_ref() else {
            return Vec::new();
        };
        monitor_entries_of(
            &monitors.list(),
            &monitors.firings(),
            letibot_sessionlog::registry::now_ms(),
        )
    }

    /// **One job, as `/job` draws it** — `release-build · j65  running  4096 bytes  cargo test`,
    /// and `j65  running  …` for a job nobody named.
    ///
    /// One function so the listing and the pane's own rows cannot spell a name two ways, and
    /// so the row is assertable without a host. The id is always drawn — it is the handle
    /// `job_output` and `/job ID` take — and a name goes in front of it, which is where a
    /// reader's eye lands first.
    fn job_row(j: &letibot_tools::exec::JobView) -> String {
        format!(
            "  {}  {}  {} bytes  {}",
            Self::job_name(j),
            j.state.word(),
            j.produced,
            Self::one_line(&j.command, 60)
        )
    }

    /// **What a job is called on a row**: `release-build · j65`, or just `j65` when nobody
    /// named it.
    ///
    /// The rule itself is [`letibot_tools::exec::JobView::label`] — one spelling for the
    /// daemon's listing, the `JobEntry` a head draws and a tool's own refusal — and the id
    /// is never dropped: a job with no name draws exactly what it drew before this existed.
    fn job_name(j: &letibot_tools::exec::JobView) -> String {
        j.label()
    }

    pub fn job_lines(&self) -> Vec<String> {
        let Some(host) = self.runtime.backend.processes() else {
            return vec!["this session has no process host, so it has no jobs".into()];
        };
        let all = host.jobs();
        // **The background ones, and whatever is still running.**
        //
        // `host.jobs()` is every process this session ever spawned — the host
        // gives each one a job id and a scope, which is how a turn-scoped reap
        // finds them. That is right for the host and wrong for this listing: the
        // operator ran `/job` after a turn and got **three hundred lines** of
        // finished `grep`, `cat` and `sed` calls, one per command the model had
        // run all session. Their words, 2026-09-20: *"some fuckery with jobs"*,
        // *"hundreds of lines"*.
        //
        // `background: Option<Backgrounding>` is the field that already draws
        // this distinction — its own doc says "or `None` if it never was" — and
        // a still-running job is kept whether or not anybody backgrounded it,
        // because that is the one ctrl-o moves and the one worth interrupting.
        // The empty-case sentence below has always said this listing is about
        // background jobs; now it is.
        let jobs: Vec<_> = all.iter().filter(|j| Self::worth_listing(j)).collect();
        if jobs.is_empty() {
            return vec!["no jobs. The model backgrounds a command with bash's `background: true`; ctrl-o moves the running one.".into()];
        }
        let mut out = vec![format!("{} job(s) this session has started.", jobs.len())];
        out.push(String::new());
        for j in &jobs {
            out.push(Self::job_row(j));
        }
        out.push(String::new());
        // The ones left out are counted, not hidden: a filtered listing that does
        // not say it filtered is a listing the reader draws wrong conclusions from.
        let hidden = all.len() - jobs.len();
        if hidden > 0 {
            out.push(format!(
                "{hidden} finished foreground command(s) not shown — every command runs \
                 in a job scope, and only the backgrounded and the still-running are \
                 jobs anybody acts on."
            ));
        }
        out.push("`/job ID` reads what one wrote.".into());
        out
    }

    /// **A background job's retained output, for the operator not the model.**
    ///
    /// The jobs pane lists what is running and how many bytes it produced, and
    /// until now that was all: the only way to read the bytes it was counting was
    /// to ask the model to call `job_output`. The operator, looking straight at
    /// the row: *"i go to jobs panel and no way to get job output"*.
    ///
    /// The same read `job_output` does — `ProcessHost::output` over the capture
    /// ring — reached by a slash reply rather than a tool call, so it needs no new
    /// protocol frame and no version bump.
    pub fn job_output(&self, job: &str, offset: u64, limit: usize) -> Result<Vec<String>, String> {
        let Some(host) = self.runtime.backend.processes() else {
            return Err("this session has no process host, so it has no jobs".into());
        };
        let jid = letibot_tools::exec::JobId(job.to_string());
        let Some(view) = host.job(&jid) else {
            return Err(format!(
                "no job `{job}` here; `/job` with no argument lists them"
            ));
        };
        let slice = host
            .output(&jid, offset, limit)
            .map_err(|e| e.to_string())?;

        // A job that has written nothing is not an empty answer about its output:
        // whether it is still running decides what the silence means. The same
        // distinction `job_output` draws, in the same words.
        // **A job whose output was redirected has, and its window must say so** — R41's
        // case, which the MODEL's `job_output` already answers
        // (`letibot_tools::builtins::jobs`) and which this path still got wrong: the head
        // read *"wrote nothing at all"* about a build that wrote megabytes to a file.
        if slice.produced == 0
            && let Some(path) = letibot_tools::builtins::output_redirect_path(&view.command)
        {
            return Ok(vec![format!(
                "`{job}`'s output goes to `{path}`, not to its window — the capture is empty \
                 by construction ({}). Read the file, or `tail -n` it.",
                view.state.word()
            )]);
        }
        if slice.produced == 0 {
            return Ok(vec![if view.state.is_running() {
                format!("`{job}` is still running and has written nothing yet.")
            } else {
                format!("`{job}` {} and wrote nothing at all.", view.state.word())
            }]);
        }

        let mut out: Vec<String> = progress_lines(&slice.text());
        out.push(String::new());
        out.push(format!(
            "[{} — bytes {}..{} of {} produced{}]",
            view.state.word(),
            slice.from,
            slice.to,
            slice.produced,
            if slice.dropped > 0 {
                format!(", {} dropped off the front", slice.dropped)
            } else {
                String::new()
            }
        ));
        if slice.to < slice.produced {
            out.push(format!("more: /job {job} --offset {}", slice.to));
        }
        Ok(out)
    }

    /// **What this conversation can actually call**, for `/tools`.
    ///
    /// Two lists, and the gap between them is the point. The REGISTRY is what this
    /// daemon seated when it opened the session; the PROMPT is what message zero
    /// announces, and it is fixed the moment a transcript starts — a resume
    /// replays the stored prefix because the stored tokens were produced under it.
    ///
    /// So a daemon restarted with a new tool has it seated and unannounced, and the
    /// model cannot call a tool it has never been told about however the banner
    /// reads. That gap cost the operator an hour once — *"i did letibot --stop and
    /// leticode --continue but still no exec"* — and the banner was no help,
    /// because the banner is computed from the registry.
    ///
    /// This names it in the one place somebody would look.
    pub fn tools_lines(&self) -> Vec<String> {
        let schemas = self.runtime.registry.schemas();
        let announced = tool_names(&self.prefix.tools_json);
        let seated: std::collections::BTreeSet<String> =
            schemas.iter().map(|s| s.name.clone()).collect();

        let mut out = vec![format!("{} tool(s) seated in this session.", schemas.len())];
        out.push(String::new());
        let mut by_name = schemas;
        by_name.sort_by(|a, b| a.name.cmp(&b.name));
        for s in &by_name {
            // A tool the prompt has never heard of is marked where the eye already
            // is, rather than only in a footnote below the list.
            let mark = if announced.contains(&s.name) {
                "  "
            } else {
                "! "
            };
            out.push(format!(
                "{mark}{:<14} {:<8} {}",
                s.name,
                format!("({})", s.access.as_str()),
                first_sentence(&s.description),
            ));
        }
        out.push(String::new());

        let missing: Vec<String> = seated.difference(&announced).cloned().collect();
        let stale: Vec<String> = announced.difference(&seated).cloned().collect();
        if missing.is_empty() && stale.is_empty() {
            out.push(
                "The prompt this conversation speaks announces exactly these, so \
                 everything seated is callable."
                    .into(),
            );
            return out;
        }
        if !missing.is_empty() {
            out.push(format!(
                "! {} seated but NOT ANNOUNCED: {}.",
                missing.len(),
                missing.join(", ")
            ));
            out.push(
                "  Message zero is fixed when a transcript starts, so a tool seated \
                 after this conversation began is one the model has never been told \
                 about — it cannot call it, whatever the banner says."
                    .into(),
            );
            out.push(
                "  `/compact` now forks onto the seated prompt and picks them up; \
                 `/reseat` does the same without waiting for the context to fill, and \
                 without summarising."
                    .into(),
            );
        }
        if !stale.is_empty() {
            out.push(format!(
                "! {} announced but NO LONGER SEATED: {}. The model may call these and \
                 will be told the tool is unknown.",
                stale.len(),
                stale.join(", ")
            ));
        }
        out
    }

    /// What answers this session's turns right now, for `/models`.
    pub fn provider_line(&self) -> String {
        match &self.provider {
            None => format!(
                "local — {} at {}",
                self.cfg.model,
                self.cfg.endpoint.authority()
            ),
            Some(p) => format!("{}/{} (metered)", p.name(), p.model()),
        }
    }

    /// **This session's model, spelled the way `/models` takes it** — `local`, `name`,
    /// or `name/model`.
    ///
    /// Two readers, and both are asking *which model is this session on* rather than
    /// *what is answering it*: the session row ([`Harness::persist_provider_choice`],
    /// where a restart reads it back through the same door) and the retry loop's
    /// fallback, which must not spend a name on the model that just refused the round.
    /// [`Harness::provider_line`] is the other question — the one a person reads, which
    /// names the endpoint and says METERED — and the two are deliberately not merged.
    pub fn model_name(&self) -> String {
        match &self.cfg.provider {
            None => "local".to_string(),
            Some(pc) => match &pc.model {
                Some(m) => format!("{}/{m}", pc.name),
                None => pc.name.clone(),
            },
        }
    }

    /// The session is over: let the backend release what it holds and say where
    /// its work went. A host backend says nothing; a firecode one brings its VM
    /// down and names the sibling directory.
    pub fn close_backend(&self) -> Option<String> {
        // The watchers first: their threads hold weak handles and re-check this
        // flag at most one wait chunk after close, so a session that closes does
        // not keep its host — or its cgroups — alive behind a blocked thread.
        if let Some(w) = &self.job_watch {
            w.stop();
        }
        self.runtime.backend.close()
    }

    /// **The jobs still running in this session's VM as it is closed** — which die with it,
    /// unannounced until this (TODO.md R92.1, second half). A VM child's backend is closed right
    /// after its first turn (`HarnessTaskRunner::run`); a job it backgrounded and did not wait on
    /// was killed with the guest, its watcher stopped first, so no settlement ever came and its
    /// row read `running` for ever.
    ///
    /// Called just before [`Harness::close_backend`]: returns the jobs, and writes each one's row
    /// as `lost with its VM` (and delivered — the parent is told, in the answer) so the store does
    /// not remind anyone of it after a restart. Empty when the backend runs no processes.
    pub fn jobs_lost_with_the_vm(&self) -> Vec<letibot_tools::exec::JobView> {
        let Some(host) = self.runtime.backend.processes() else {
            return Vec::new();
        };
        let lost: Vec<_> = host
            .jobs()
            .into_iter()
            .filter(|j| j.state.is_running())
            .collect();
        if lost.is_empty() {
            return lost;
        }
        if let Some(store) = self.cfg.store.as_ref().and_then(|p| Store::open(p).ok()) {
            for j in &lost {
                let row = JobRecord {
                    handle: j.id.0.clone(),
                    command: j.command.clone(),
                    how: String::new(),
                    state: VM_LOST.to_string(),
                    produced: j.produced,
                    elapsed_ms: j.elapsed.as_millis() as u64,
                    redirect: None,
                };
                let _ = store.put_job(&self.cfg.session_id, &row);
                let _ = store.deliver_job(&self.cfg.session_id, &row.handle);
            }
        }
        lost
    }

    /// The session's own scope, for a monitor the daemon declares on the session's
    /// behalf — the flowy listener. `None` when the backend cannot start processes,
    /// which is also when there are no monitors to declare it in.
    pub fn session_scope(&self) -> Option<letibot_tools::exec::ScopeId> {
        self.runtime
            .backend
            .processes()?
            .scope_for(letibot_tools::exec::ScopeKind::Session, None)
            .ok()
    }

    /// The cursor into settled monitors, shared with this session's steering source
    /// so a firing is delivered exactly once however it is noticed.
    pub fn monitor_cursor(&self) -> &Arc<AtomicUsize> {
        &self.monitor_cursor
    }

    /// Say that this session's monitor wake is armed, so the disclosure stops
    /// saying it is a poll.
    ///
    /// A setter rather than something the harness works out, because arming is the
    /// **daemon's** act — the harness has no bell to ring and no thread to block in
    /// — and a harness that claimed a wake it had not been given would be the
    /// banner defect this file has already paid for four times.
    pub fn declare_monitor_wake(&mut self) {
        self.wiring.monitor_wake = true;
    }

    /// **The shared tail has taken a turn on this harness** — `turn_tail::run_turn`, once per door,
    /// before the door's own act.
    ///
    /// It says only that there is a turn to end, not that one has started speaking: what starts the
    /// clock is the turn's first words ([`Harness::begin_turn_clock`]), and [`Harness::end_turn_clock`]
    /// clears this again. A turn taken and never ended would be exactly the state
    /// `Harness::system_update`'s own rounds would leave behind, which is why it is the tail's act
    /// rather than every caller's.
    pub(crate) fn turn_taken(&mut self) {
        self.turn_open = true;
    }

    /// **A prompt has arrived: this is where the whole turn's clock starts.**
    ///
    /// Called once per prompt, never per round — the engine's `turn_began_ms` is then carried on
    /// every `TurnStarted` that prompt produces, which is what stops a head restarting its clock
    /// at each round. The engine field is private to this module and this is the only writer, so
    /// the two cannot drift out of step.
    ///
    /// **Idempotent within one prompt**, and that is load-bearing now that the two callers are the
    /// places a turn actually speaks: [`Harness::submit_items_then`] (which a wall-continuation
    /// re-enters, one prompt later, with this still running) and [`Harness::run_rounds`] (which the
    /// operator's `!` line reaches with its rows already in). A second call leaves the start where
    /// the first put it, so the promise above holds without either caller having to know.
    ///
    /// **And it is NOT called by the door.** `turn_tail::run_turn` used to start it before the
    /// door's own act, which meant a door that found nothing to say — `Harness::wake` with no notice
    /// behind it, `Ok(None)` — published `waiting on: turn` for a turn that never ran; a `wait`
    /// blocked on that session armed on it and answered `0 — the turn ended` about a session that
    /// had run nothing at all. MEASURED 2026-10-26 as the bridge's end-to-end flake, one run in
    /// four: the empty wake was the daemon's own startup ring
    /// (`Sessions::put_a_restored_session_to_work`) draining on the worker's first pass, on the
    /// session that had never turned.
    pub fn begin_turn_clock(&mut self, began_ms: u64) {
        // **A settlement taken for this turn is no longer owed** — the words are in the rows this
        // turn opens with, whether or not the clock below starts. See
        // `JobWatchers::settlement_in_flight`.
        if let Some(w) = &self.job_watch {
            w.turn_began();
        }
        // **Only a turn the shared tail is running can start a clock** — see the field. A
        // `run_rounds` reached outside one (`Harness::system_update`'s, from a compaction or the
        // `m1` script) has no `end_turn_clock` behind it, and a clock nobody clears is a session
        // that reads `waiting on: turn` for the life of the daemon. And a door that has not spoken
        // yet has not started a turn either: the clock waits for the first words.
        if !self.turn_open || self.prompt_in_flight.load(Ordering::SeqCst) {
            return;
        }
        self.turn_began_ms = Some(began_ms);
        self.engine.turn_began_ms = Some(began_ms);
        // **And a prompt is in flight from here until the loop returns** — see the field. The
        // engine's rounds come and go inside this, which is exactly why a reader that times the
        // round events reads a working session as idle at every boundary.
        self.prompt_in_flight.store(true, Ordering::SeqCst);
        // **And the room is told the session is WORKING** — the beat, run by the daemon and not by
        // the model. A model that must remember to call `beat` is thinking about its work, and the
        // ninety-second lease would take its item while it works; the turn clock is the daemon's,
        // so this is where *working* starts and [`Harness::end_turn_clock`] is where it stops.
        // Idempotent, and a session with no room pays one `Option` test.
        if let Some(room) = &self.room {
            room.turn_started();
        }
        self.publish_activity();
    }

    /// The turn is over: stop publishing a start, so a later round that belongs to a NEW prompt
    /// cannot inherit this one's clock.
    ///
    /// **And this is where a switch the operator made during the turn takes effect** — see
    /// [`Harness::set_provider`], and [`Harness::apply_pending_switch`], which runs after the
    /// span is marked closed and before anything else reads what answers. Every way out of a
    /// turn comes through here (`turn_tail::run_turn`: a final answer, an interrupt, a failure,
    /// a wall, and the door that said nothing at all), which is exactly why the pending switch
    /// is applied HERE and not on a success path.
    pub fn end_turn_clock(&mut self) {
        self.turn_began_ms = None;
        self.engine.turn_began_ms = None;
        // The turn the tail took is over — see the field. Cleared here rather than in `run_turn`
        // because this is the call every path out of the tail goes through, the empty one included.
        self.turn_open = false;
        // The prompt's loop has returned — a final answer, an interrupt, a failure, or a wall that
        // ended it. Whatever the session is waiting on from here is said by the rest of the facts
        // (`delivery`, a job, a monitor, a child, a card) and not by a turn.
        self.prompt_in_flight.store(false, Ordering::SeqCst);
        // **And a turn that ends owes nothing** — one that never opened the batch it took hands the
        // results back with `put_back`, and this keeps a take from outliving its turn.
        if let Some(w) = &self.job_watch {
            w.turn_began();
        }
        // **After the flag, deliberately**: the switch is applied through the same door a
        // between-turns switch takes, and a gate that saw the span still open would pend it again
        // for ever.
        // **The turn is over, so the beats stop.** The lease now counts from the last one, which is
        // exactly the fact it measures: a session whose turn ended is alive and not working on the
        // item — the operator's *"so they don't occupy it for ever"*, without the model having to
        // do anything.
        if let Some(room) = &self.room {
            room.turn_ended();
        }
        self.apply_pending_switch();
        self.publish_activity();
    }

    /// **The switch that was waiting for the turn to end, taken** — the other half of
    /// [`Harness::set_provider`]'s gate.
    ///
    /// `None` is the ordinary case and costs nothing. A failure — a key that no longer resolves,
    /// a provider that will not build — leaves the session on the model it is on, which is a
    /// working session, and says so under its own code: a request the operator made that did not
    /// take is theirs to correct, not a fault of the session's.
    fn apply_pending_switch(&mut self) {
        let Some(pending) = self.pending_switch.take() else {
            return;
        };
        match self.set_provider_now(pending.choice) {
            Ok(line) => {
                // **The session row follows it**, the same as a switch made between turns
                // (`Harness::apply_model_choice`): the row is what a restart resumes on, and a
                // deferred switch that only reached this process would expire with it.
                if let Err(why) = self.persist_provider_choice() {
                    self.hub.publish(SessionEvent::Warning {
                        code: "model_switch_applied".into(),
                        detail: format!("{line}. {why}"),
                        compaction: None,
                    });
                    return;
                }
                self.hub.publish(SessionEvent::Warning {
                    code: "model_switch_applied".into(),
                    detail: format!("the turn has ended, so the switch stands: {line}"),
                    compaction: None,
                });
            }
            Err(e) => {
                self.hub.publish(SessionEvent::Warning {
                    code: "model_switch_not_applied".into(),
                    detail: format!(
                        "the switch to `{}` was waiting for this turn to end and could not be \
                         made: {e}. This session stays on `{}`.",
                        pending.model,
                        self.model_name()
                    ),
                    compaction: None,
                });
            }
        }
    }

    /// **The plan, as a nudge — or nothing when there is no plan to nudge about.**
    ///
    /// The finding half of [`Harness::close_the_turn`] on its own: `close_the_turn` joins
    /// this with the intent ledger's own finding, because a turn boundary is where both are
    /// checked and one nudge budget has to carry both. This is the same text with nothing
    /// joined to it, for the caller that has no turn boundary to hang it on.
    /// **The operator's half of the board** — see `Sessions::dispatch`'s `SetOperatorTodos`.
    ///
    /// The board is one list with two authors; this replaces the operator's half and leaves the
    /// model's alone. Anything that reads the board afterwards — the pane, the model's prompt, and
    /// `nag_notice` above — sees both, which is the whole of the design.
    ///
    /// **`moved` is the one thing that reaches the model's half**, and it is a STATE and never a
    /// membership: `/todo postpone|resume <the row's words>` on a row the model wrote, which the
    /// operator can silence and can never add, drop or re-word. It is resolved by
    /// `TodoBoard::set_states` — the words are the key — and a name that resolves to nothing (the
    /// model rewrote its plan between the head's read and this write) is the daemon's log and the
    /// union below is the correction.
    ///
    /// **The version bump IS the announcement.** `set_operator` raises the board's version, and
    /// `flush_todos` — which runs at every turn boundary — publishes a `TodosUpdated` and persists
    /// when the version has moved. So the operator's rows reach every head and the store by the same
    /// path the model's do, with no second mechanism.
    pub fn set_operator_todos(
        &mut self,
        items: Vec<letibot_tokencore::store::TodoItem>,
        moved: &[(String, letibot_tokencore::store::TodoStatus)],
    ) {
        self.todos.set_operator(items);
        // **And the state moves that are not the head's to make.** `/todo postpone|resume` names a
        // row the MODEL wrote by its words; the board resolves them against the model's half and
        // sets the status, which is the one thing the operator can change about a row they did not
        // write — never its membership, its words or its author.
        //
        // **A refusal is the daemon's log and not the operator's silence.** The head resolved these
        // names against the union it was looking at; a model that rewrote its plan in between makes
        // a name resolve to nothing, and there is no frame here to say so on — the union this write
        // publishes IS the correction, and the head draws it.
        if !moved.is_empty()
            && let Err(why) = self.todos.set_states(moved)
        {
            eprintln!("  todos: a state move did not land: {why}");
            // The head that sent it is told too (R67 item 15): the union this write publishes is
            // the correction, and this is the sentence that says why the row did not move.
            self.import_note(
                "todo_move_unresolved",
                format!(
                    "a `/todo` state move did not land: {why}. The board below is the plan as it \
                     stands now."
                ),
            );
        }
        // **AND PERSIST IT AND TELL EVERY HEAD, which is the half the version bump alone does not
        // do.** `flush_todos` is version-gated and runs at every TURN BOUNDARY — so on a session
        // where no turn ever runs again, the operator's rows would sit on the board unpublished and
        // unwritten, and `Sessions::dispatch` is the one place that can flush them at the moment they
        // arrive. MEASURED as an omission by reading the path rather than by watching it fail: the
        // bump makes `flush_todos` willing, and nothing made it happen.
        //
        // The row does not reach the model's TOOL REPLY — that is `todo`'s own — so nothing here is
        // a turn.
        self.flush_todos_or_say("the operator's row");
    }

    /// **The children this daemon is running, as the plan-check reads them.**
    ///
    /// The fact a board cannot hold, taken where it lives: the task journal says which children are
    /// still running and the session registry says how long each has been, and the child's own hub
    /// says what it last showed. `crate::sessions::child_sessions` is the one builder — the daemon's
    /// worker arm calls the same function for the clock it holds, so the two readers of one plan
    /// cannot come to different answers about a child.
    ///
    /// **Taken per ask rather than held**, because it is the one input to the check that moves
    /// while nothing else does: a board nobody touched reads differently ten minutes later, and a
    /// cached snapshot would be a check that cannot see a child stop.
    fn child_sessions(&self) -> letibot_tools::builtins::todo::ChildSessions {
        crate::sessions::child_sessions(
            &self.session_registry,
            &self.tasks,
            letibot_sessionlog::registry::now_ms(),
        )
    }

    /// **The jobs this session is running, as the plan-check reads them.**
    ///
    /// The other fact a board cannot hold, and the one the check has to know to stay SILENT about a
    /// row that waits on a job ([`letibot_tokencore::store::TodoNeed::Job`]): while the job runs the
    /// row is not work the check may speak about, and the moment it ends the row is.
    ///
    /// **Asked PER HANDLE the plan names, through [`Harness::job_fate`]** — the same lookup the
    /// `[todo] … is due` firing reads through, so the two readers of one handle cannot come to
    /// different answers about it, and a handle this session has never heard of answers `Unknown`,
    /// which is a job that ENDED. A snapshot built from a listing instead would be a second way to
    /// ask *is this running* — and the second way would be the one that disagreed after a restart,
    /// where the live table is empty and only the record knows what became of the job.
    ///
    /// **Taken per ask rather than held**, for `child_sessions`' reason: a job ends while nothing
    /// else moves, and a cached snapshot would be a check that cannot see it stop.
    pub(crate) fn running_jobs(
        &self,
        rows: &[TodoItem],
    ) -> letibot_tools::builtins::todo::RunningJobs {
        let mut jobs = letibot_tools::builtins::todo::RunningJobs::none();
        for row in rows {
            for need in &row.needs {
                // **A monitor still watching** is the job's sibling: the row waits on it, and the
                // check is quiet about it, until it ends.
                if let letibot_tokencore::store::TodoNeed::Monitor { name } = need {
                    if self
                        .monitors
                        .as_ref()
                        .is_some_and(|m| m.get(name).is_some())
                    {
                        jobs = jobs.watching(name.clone());
                    }
                    continue;
                }
                let letibot_tokencore::store::TodoNeed::Job { handle } = need else {
                    continue;
                };
                if matches!(self.job_fate(handle), JobFate::Running) {
                    jobs = jobs.running(handle.clone());
                }
            }
        }
        // **And the grace after an ending** — a job or child a row waits on that has ended, whose
        // completion the model has not digested yet: still queued or held, or delivered during the
        // turn whose end is being checked. Read off the watcher set's own record of what it
        // delivered when (`JobWatchers::grace_left`), with R79's starvation cap as the backstop.
        // A running job is skipped: it is `running` above, and the grace is for after.
        if let Some(w) = &self.job_watch {
            jobs = graced_endings(
                jobs,
                rows,
                w,
                self.cfg.held_results_cap,
                std::time::SystemTime::now(),
            );
        }
        // **And what this session COULD wait on, for the check's offer of `wait_on`** — every job
        // its own process table says is running, and every child it spawned that the journal says
        // is still going. Adding a running job that no row names changes no row's reading (a row's
        // edge asks about its own handle), and the live table is the first look `job_fate` takes,
        // so the two cannot disagree about a job that runs. The children are this session's alone —
        // `child_sessions` is the daemon's whole journal — because an offer to wait on somebody
        // else's child is an offer `wait_on` would refuse.
        if let Some(host) = self.runtime.backend.processes() {
            for view in host.jobs() {
                if view.state.is_running() {
                    let about = match &view.slug {
                        Some(slug) => format!("{slug}: {}", view.command),
                        None => view.command.clone(),
                    };
                    jobs = jobs.about(view.id.0.clone(), about).running(view.id.0);
                }
            }
        }
        for entry in self.tasks.snapshot() {
            if entry.parent == self.cfg.session_id
                && matches!(entry.state.as_str(), "opening" | "running")
            {
                jobs = jobs
                    .about(entry.name.clone(), entry.prompt)
                    .subagent(entry.name);
            }
        }
        jobs
    }

    /// **The `[todo check]` text, or nothing** — and a row the operator has postponed is not part
    /// of it.
    ///
    /// The rows are read through `sessions::the_plan_as_checked`, which is the same reading the
    /// arming decision takes: a plan whose only unfinished row has been set aside is a plan with
    /// nothing to check, and it must not produce this sentence **even if the clock was armed
    /// before the row was postponed** — which is the one way a postponed row could still be
    /// nagged about, and the reason this is filtered here rather than left to the arming alone.
    /// One predicate, so the clock and the sentence cannot disagree about what the plan is.
    ///
    /// **And the row this names is remembered, so the question does not wander.** The check picks a
    /// row once and names it again until it stops being askable — a row the MODEL adds elsewhere on
    /// the plan cannot change which row it is being held to, and the operator's ready rows come
    /// before it (R77, `unfinished_plan_for`) — and a row that becomes `InProgress` changes the
    /// QUESTION rather than the choice. The clock keeps the TEXT, which is what the ladder counts
    /// repeats of; the row itself is this harness's to remember, because the text cannot say which
    /// row it named.
    ///
    /// **Reading it does not remember it** (R77). Both delivery doors read this once to record the
    /// notice as said and then run [`Harness::nag_turn`], which reads it again — and when this
    /// wrote `nag_choice` the second reading already counted the first as *asked*, so the model's
    /// FIRST check of any row said *"you were asked about this one already"*, and the first check
    /// after an operator's add could never say they added it. The choice is committed by
    /// `nag_turn`, the one place a check is actually delivered.
    pub fn nag_notice(&self) -> Option<String> {
        self.nag_notice_naming().map(|(text, _)| text)
    }

    /// **What the idle clock counts a delivered check AS** — the plan's sentence read the way the
    /// clock's own arming reads it (`sessions::nag_should_arm`: the narrowed rows, no sticky
    /// choice, no stale-prompt sentence), so *the same notice again* means the same plan and not
    /// the same wording (R77).
    ///
    /// The wording moves on its own: the first check of a row names it plainly (or says the
    /// operator added it) and the next says *"you were asked about this one already"*. Recording
    /// the WORDING as said made the second delivery a different notice from the arming's reading
    /// of an unchanged plan, so every check after it came due at the base window — the metronome
    /// the ladder exists to prevent. The delivery doors record this instead of the text they send.
    ///
    /// **The queue's census is NOT in here** — it is a door of its own (`Door::Census`,
    /// `Sessions::deliver_due_censuses`), recorded against its own anchor because its sentence names
    /// the age of the oldest row and so is a different notice at every delivery. What this returns
    /// is the plan's sentence, which is what the ladder counts repeats of; `None` is the right
    /// answer for a project with nothing on its board and eight rows stuck in its queue.
    pub fn nag_said_as(&self) -> Option<String> {
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        unfinished_plan(&askable, &board, &children, &jobs)
    }

    /// [`Harness::nag_notice`] and the row it named — `None` for the second half when the notice is
    /// only the stale-prompt sentence, which names no row.
    fn nag_notice_naming(&self) -> Option<(String, Option<String>)> {
        // **The board and the rows it may speak about, both** — the second is the narrowing
        // (`the_plan_as_checked`, the ONE predicate) and the first is what a row's edges resolve
        // against, since a need is met by a row that is DONE and a done row is exactly what the
        // narrowing takes out. `unfinished_plan_for` says why they are two arguments.
        //
        // **And the children, which are the third thing a row can wait on** and the one fact none
        // of the above holds: `child_sessions` is the daemon's snapshot of what is running, taken
        // here so a child that has gone quiet since the last check is seen to have.
        //
        // **And the jobs, which are the fourth** — the same argument one layer down, and the one the
        // operator's own long jobs made: a row that waits on a job that is running is not work this
        // check may speak about, so the snapshot is taken here too rather than remembered.
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        // **The plan's text, as a value rather than an early return**, because this notice now
        // has a second thing it may have to say and a completed plan is no reason to say
        // neither. `nag_should_arm` compares the TEXT, so a notice that gains a sentence is a
        // new thing to say at the base window and an unchanged one is a repeat at the cap —
        // the ladder needs no change at all for this.
        let plan = unfinished_plan_for(
            &askable,
            &board,
            self.nag_choice.as_deref(),
            &children,
            &jobs,
        );
        let (plan, named) = match plan {
            Some((text, named)) => (Some(text), Some(named)),
            None => (None, None),
        };
        // **And the prompt this session is speaking, when it is not this daemon's.**
        //
        // The sentence is composed once, at open, by the fingerprint comparison and left in the
        // registry for the ATTACH path — because a warning published before a head arrives is
        // filed `Placed::Before` and never drawn. The head is not the only reader it needs: the
        // model cannot see it either, so a session can spend an evening speaking a prompt its
        // daemon would not compose, with the model unable to name the problem. This is the
        // model's copy, and it ends with what to DO rather than with the diagnosis — the same
        // shape `[todo check]` uses, for the same reason: the rule travels with the notice
        // instead of living somewhere the model has to remember to look.
        //
        // Nothing clears this but a re-seat, and that is deliberate: `set_stale_prefix(…, None)`
        // is written by the fork onto a new prompt, so the reminder stops being true exactly
        // when the person acts — the loop closes without anybody having to remember to close it.
        //
        // **Told ONCE, and as information** (see `stale_told`): the person has the choice on their
        // screen as a card that stays until the prompt is current, so the model is told the fact
        // and that the decision is not its to press — and is not told it again.
        let stale = self
            .stale_untold()
            .map(|said| stale_notice_for_the_model(&said));
        let text = match (plan, stale) {
            (None, None) => None,
            (Some(plan), None) => Some(plan),
            (None, Some(stale)) => Some(stale),
            (Some(plan), Some(stale)) => Some(format!("{plan}\n\n{stale}")),
        };
        text.map(|text| (text, named))
    }

    /// **The stale-prompt sentence, when the model has not been told it yet** — `None` when the
    /// prompt is current or the model already heard this very sentence (see `stale_told`).
    fn stale_untold(&self) -> Option<String> {
        self.session_registry
            .stale_prefix(&self.cfg.session_id)
            .filter(|said| self.stale_told.as_deref() != Some(said.as_str()))
    }

    /// **The plan's nudge as a turn of its own, when nothing else is happening.**
    ///
    /// The scheduling question the operator raised: *"we have to think about scheduling.
    /// maybe wait for a timeout actually. so send it when model is idling"* — and, on the
    /// failure the old arrangement had, *"but certainly not after my message."*
    ///
    /// What this gives the caller is a way to deliver the check **without a turn boundary**.
    /// The old path could only run it inside the loop, at the end of a turn (`nudges_left`,
    /// one per user turn), so a session that went idle with an unfinished plan was nudged
    /// once and then never again — while a session in constant conversation was nudged after
    /// every exchange, which is the overkill the operator reported. Neither is a schedule;
    /// both are consequences of there being nowhere else to put the check.
    ///
    /// The text goes in as `Speaker::Agent`, exactly as [`Harness::wake`] and
    /// [`Harness::continue_after_wall`] do: the harness talking to itself, never readable as
    /// the operator's words and never an authorisation.
    ///
    /// `Ok(None)` when there is nothing to say — an empty or finished plan — which is a real
    /// outcome and not a failure: a caller that asked has its answer, and no turn is spent
    /// on a nudge with no finding behind it.
    pub fn nag_turn(&mut self) -> Result<Option<Reply>, HarnessError> {
        let Some((mut text, named)) = self.nag_notice_naming() else {
            return Ok(None);
        };
        self.meter_nag(named.as_deref());
        // **R79: a plan-check is a turn the model takes anyway, so it carries the held batch** —
        // which is what reaches a session that has a plan and nobody typing.
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        // **The choice is remembered HERE, where the check is delivered** — see `nag_notice` for
        // why reading the notice must not.
        if let Some(named) = named {
            self.nag_choice = Some(named);
        }
        // **And the stale prompt counts as told** — once, so the next check does not repeat it.
        if let Some(said) = self.stale_untold() {
            self.stale_told = Some(said);
        }
        // **The wall is checked BEFORE the send — `submit_as_a_normal_session`'s
        // pre-flight, from the same function.** Insurance, not the load-bearing
        // fix: a refused nag is still classified as the wall (see the `Http` arm
        // in `run_rounds`) and recovered by the door that answers a refusal, so
        // this exists to spare the round-trip the daemon's own numbers can
        // predict — a nag is the cheapest turn the session takes, and the most
        // pointless one to spend on a prompt already known not to fit. MEASURED
        // 2026-10-09: the LAST of the thirty refusals on `s-1789919514688401228`
        // was attributed to `todo check` — this door.
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
        .map(Some)
    }

    // ---- the metrics instruments' harness half (TODO R81, `crate::instruments`) ----------
    //
    // Each call site in this file is one line; what the line needs from the harness is here, in
    // one block, so a merge that moves a site moves one line.

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

    /// **A board row's status by its words**, as a word — for a nag's follow-up row (TODO R81):
    /// the turn a check opened touched the row it named when this answers differently after it.
    /// `None` is a row no longer on the board, which is a touch too.
    pub(crate) fn row_status(&self, content: &str) -> Option<String> {
        self.todos
            .snapshot()
            .into_iter()
            .find(|t| t.content == content)
            .map(|t| format!("{:?}", t.status).to_lowercase())
    }

    /// Every board row as `(words, status)` — the nag follow-up's read (TODO R81).
    pub(crate) fn todo_snapshot_words(&self) -> Vec<(String, String)> {
        self.todos
            .snapshot()
            .into_iter()
            .map(|t| (t.content, format!("{:?}", t.status).to_lowercase()))
            .collect()
    }

    fn row_is_operators(&self, content: &str) -> bool {
        self.todos
            .snapshot()
            .iter()
            .any(|t| t.content == content && t.by == letibot_tokencore::store::TodoBy::Operator)
    }

    /// **What this session's model is told about the queues: a repository that declares no gate.**
    ///
    /// For each repository a branch from this session's tree is waiting in, whose target is held
    /// `blocked` because its `AGENTS.md` has no `Merge gate` section — once per daemon. The
    /// sentence names the repository and what to do: `merge_gate` for the choices, the operator to
    /// pick, the section committed on the target.
    /// **Arm the watches on a pull request this session just opened** (`pr_watch`), through the
    /// operator's door (the model did not ask for them, so no gate is owed them), and answer the
    /// sentence the model reads. A watch already armed under the same name is left as it is.
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

    /// **A plugin layer's look at a tool result** (`docs/plugins-plan.md` §4.2): the text the model
    /// is about to read, and anything the layer asked to watch.
    ///
    /// It changes the payload in place, because that is the one string every later reader uses —
    /// the progress encoder, the transcript row and the store — so a rewrite that ran after any of
    /// them would be a result the model read and nobody recorded.
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

    /// **Arms a monitor a layer asked for**, through the operator's door — the same one
    /// [`Harness::watch_opened_pr`] uses, and for the same reason: nobody asked this session's
    /// model for the watch, so no gate is owed it, and the monitor tool's own rules (coalescing,
    /// the already-watched refusal, the TTL caps) apply unchanged.
    ///
    /// The answer is a sentence appended to the call's result, so the model reads what is now
    /// watching at the moment it caused it.
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

    /// **Whether the queues' news is this daemon's to tell a model.** False under `--no-queue`
    /// ([`Config::serve_queues`]), and it is the whole of the model-facing half of that flag: a
    /// daemon that holds no lease composes no sentence about a lease — not the emergency
    /// ([`Harness::queue_emergency_notices`]), not the project's missing gate
    /// ([`Harness::queue_notices`]), not a branch handed back to it
    /// ([`Harness::handover_notices`]) — because every one of them would be about work another
    /// daemon owns, and a package worker told to fix a red `main` is the defect the flag exists
    /// for (2026-10-26). The heads still draw the queue's alerts from the watcher; only the
    /// model's copy is withheld.
    fn queue_news_is_mine(&self) -> bool {
        self.cfg.serve_queues
    }

    /// **This project's stuck rows, as the idle check reads them** — the census
    /// (`mergequeue::stuck`), or `None` when this session is not the project's, when the queue is
    /// not this daemon's to speak about, or when nothing is stuck.
    ///
    /// **The project's, by the same test the emergency uses** — a root session of this workspace's
    /// repositories and never the review queue's own — because a count of rows is news only to the
    /// session that can act on them, and a subagent told *eight rows are stuck in `main`* has been
    /// handed somebody else's work. `queue_news_is_mine` is the `--no-queue` flag's half: a daemon
    /// that holds no lease composes no sentence about a lease.
    ///
    /// `repositories_of_workspace_cached` and not the uncached one: this is read on every turn's
    /// end (the clock re-arms there), and the answer for one workspace does not change between two
    /// of them. A session in NO repository answers `None` rather than the whole box's rows — the
    /// same rule `desk::nowhere` states, and for the same reason.
    pub fn stuck_census(&self) -> Option<crate::mergequeue::stuck::Stuck> {
        if !self.queue_news_is_mine() || self.is_the_queue() {
            return None;
        }
        let store = self.store.as_ref()?;
        let me = self.hub.session_id();
        if crate::mergequeue::root_session(store, &me) != me {
            return None;
        }
        let mine = crate::mergequeue::repositories_of_workspace_cached(&self.cfg.workspace);
        if mine.is_empty() {
            return None;
        }
        let census = crate::mergequeue::stuck::of(store, &mine).ok()?;
        (!census.is_empty()).then_some(census)
    }

    /// **What the idle check's clock needs to know about this project's queue** — one reading, and
    /// the clock's arming, the sentence and the identity all take it from here, so a schedule
    /// cannot arm for a census the sentence would not say.
    ///
    /// `says` is `Stuck::is_news` and not *there are bad rows*: a row that went bad a minute ago is
    /// the transition notice's, and the census exists for the row that STAYED bad.
    pub fn queue_news(&self) -> crate::sessions::QueueNews {
        let Some(census) = self.stuck_census() else {
            return crate::sessions::QueueNews::none();
        };
        let now = letibot_sessionlog::registry::now_ms();
        crate::sessions::QueueNews {
            says: census.is_news(now),
            matures_in: census.news_in_ms(now).map(std::time::Duration::from_millis),
        }
    }

    /// **The census as a sentence, when there is one to say** — `None` for a queue with nothing
    /// stuck, or one whose oldest row is still too young to be news.
    ///
    /// Read by `Sessions::deliver_due_censuses`, which hands it to [`Harness::census_turn`]. It is
    /// composed from the store AT THE MOMENT IT IS SAID and not held from the arming, so a row that
    /// recovered between the two is not announced as stuck.
    pub fn stuck_census_text(&self) -> Option<String> {
        let now = letibot_sessionlog::registry::now_ms();
        let census = self.stuck_census()?;
        census
            .is_news(now)
            .then(|| crate::mergequeue::notices::stuck_census(&census, now))
    }

    /// **The census as a TURN** — the sentence [`Harness::stuck_census_text`] composed, said by the
    /// harness and answered by the model.
    ///
    /// It was a NOTICE and it cost no model call, and it reached nobody for eight hours. MEASURED
    /// 2026-11-01 on the operator's daemon: a row sat in `conflict` from 02:37 to 10:52 while the
    /// census wrote its sentence every interval into a session that was not running — a mailbox,
    /// read at the model's next turn, and a quiet session has none. Their verdict on it names the
    /// defect: *"probably doesnt even reach you"*.
    ///
    /// So it goes through the door every turn goes through (`submit_spoken`, and the shared tail
    /// around it), which is what makes it a bell rather than a note: the model is woken, reads the
    /// count and the age, and can act. **What it costs is one model call per interval per project
    /// while a row is stuck** — `mergequeue::stuck::STUCK_REPEAT_MS` (two hours), against eight
    /// hours of a row nobody saw.
    ///
    /// **The held batch rides in with it**, as R79 gives it to every turn a break ends: the
    /// sentences delivered while nothing was running are the reason a quiet session has turns to
    /// take at all.
    pub fn census_turn(&mut self, text: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken(Speaker::Agent, text)
    }

    pub fn queue_notices(&mut self) -> Vec<String> {
        if !self.queue_news_is_mine() {
            return Vec::new();
        }
        let me = self.hub.session_id();
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        let healths = store.target_healths().unwrap_or_default();
        let mut out = Vec::new();
        for r in store.review_rows(None).unwrap_or_default() {
            if !(r.state.is_open() || r.state == letibot_tokencore::store::ReviewState::Approved)
                || crate::mergequeue::root_session(store, &r.session_id) != me
            {
                continue;
            }
            let Some(h) = healths.iter().find(|h| {
                h.repository == r.repository
                    && h.target == r.target
                    && h.state == letibot_tokencore::store::HealthState::Blocked
                    && h.evidence.starts_with(crate::mergequeue::WAITING_FOR_GATE)
            }) else {
                continue;
            };
            let key = format!("gate:{}", h.repository);
            if !self.gate_told.insert(key) {
                continue;
            }
            out.push(format!(
                "The merge queue cannot land anything in `{repo}`: its target `{target}` has no \
                 `Merge gate` section in AGENTS.md, so there is nothing to judge a branch by \
                 (`{branch}` is waiting there). Call `merge_gate` with repo `{repo}` to see what it \
                 suggests and ask the operator with `ask_user_question` (options = those choices; \
                 they may type their own command). Exception to the queue rule: once they pick, \
                 add the section to that repository's AGENTS.md and commit it to `{target}` \
                 yourself. The queue goes on by itself when the section is there.",
                repo = h.repository,
                target = h.target,
                branch = r.branch,
            ));
        }
        out
    }

    /// **A target emergency, told to the model of the project that has to act** (R76 item 1).
    ///
    /// The queue raised `target_red`, `queue_blocked` and `queue_needs_human` as warnings on every
    /// hub — which heads draw and no model reads. pg-noop's `main` went red on 2026-10-11 and the
    /// project's own agent never heard of it, and could not have got itself out if it had. So a
    /// ROOT session (never the review queue's own) is told, once per daemon per red tip or parked
    /// row, with the failing step and the end of its output FIRST:
    ///
    /// * its repository's target is red, timed out, or unjudged because the gate's own setup
    ///   failed (`mergequeue::serve::emergency_health`);
    /// * the review of its target's red tip is blocked or cannot decide;
    /// * a branch of its own tree is blocked in review, or its reviewer cannot decide (a branch
    ///   handed over under `merge_via_session` is [`Harness::handover_notices`]'s instead).
    ///
    /// The queue's worker rings the session (`QueueDoor::ring_repository` / `ring_project`); these
    /// are the words its wake reads.
    pub fn queue_emergency_notices(&mut self) -> Vec<String> {
        if !self.queue_news_is_mine() {
            return Vec::new();
        }
        let me = self.hub.session_id();
        if self.is_the_queue() {
            return Vec::new();
        }
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        if crate::mergequeue::root_session(store, &me) != me {
            return Vec::new();
        }
        let all_healths = store.target_healths().unwrap_or_default();
        let greens: Vec<_> = all_healths
            .iter()
            .filter(|h| h.state == letibot_tokencore::store::HealthState::Green)
            .cloned()
            .collect();
        let healths: Vec<_> = all_healths
            .into_iter()
            .filter(crate::mergequeue::serve::emergency_health)
            .collect();
        let parked: Vec<_> = store
            .review_rows(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                matches!(
                    r.state,
                    letibot_tokencore::store::ReviewState::Blocked
                        | letibot_tokencore::store::ReviewState::CannotDecide
                ) && !r.evidence.starts_with(crate::mergequeue::HANDED_TO_PROJECT)
            })
            .collect();
        // The repository this session is the project of — read only when there is news.
        // Every repository of the workspace — one, or each of a one-org project folder's.
        let mine = crate::mergequeue::repositories_of_workspace(&self.cfg.workspace);
        let mut out = Vec::new();
        // **Green again** (R92.6): an emergency this session was told about is over — said once,
        // and forgotten, so the next red of the same tip is news again (R92.31).
        for h in &greens {
            if !mine.contains(&h.repository) {
                continue;
            }
            let prefix = format!("emergency:{}:{}:", h.repository, h.target);
            let held = self.gate_told.iter().any(|k| k.starts_with(&prefix));
            self.gate_told.retain(|k| !k.starts_with(&prefix));
            let stored = store
                .untell_prefix(&format!("told:{me}:{prefix}"))
                .unwrap_or(0);
            if held || stored > 0 {
                out.push(crate::mergequeue::notices::green_again(h, &h.repository));
            }
        }
        if healths.is_empty() && parked.is_empty() {
            return out;
        }
        for h in healths {
            if !mine.contains(&h.repository)
                || !told_once(
                    &mut self.gate_told,
                    store,
                    &me,
                    format!(
                        "emergency:{}:{}:{}:{}",
                        h.repository,
                        h.target,
                        h.sha,
                        h.state.as_str()
                    ),
                )
            {
                continue;
            }
            out.push(crate::mergequeue::notices::health(&h));
        }
        for r in &parked {
            let ours = match r.kind {
                letibot_tokencore::store::ReviewKind::Target => mine.contains(&r.repository),
                letibot_tokencore::store::ReviewKind::Branch => {
                    !r.session_id.is_empty()
                        && crate::mergequeue::root_session(store, &r.session_id) == me
                }
            };
            if !ours
                || !told_once(
                    &mut self.gate_told,
                    store,
                    &me,
                    format!("parked:{}:{}:{}", r.id, r.state.as_str(), r.state_ms),
                )
            {
                continue;
            }
            out.push(crate::mergequeue::notices::parked(r));
        }
        out
    }

    /// **The reviews the queue handed to THIS project's main session** (R61 decision 4's
    /// per-project option, `[queue] merge_via_session = true`): a review that could not run, so
    /// the project gates and merges the branch itself — told once, read off the row.
    pub fn handover_notices(&mut self) -> Vec<String> {
        if !self.queue_news_is_mine() {
            return Vec::new();
        }
        let me = self.hub.session_id();
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for r in store.review_rows(None).unwrap_or_default() {
            if r.state != letibot_tokencore::store::ReviewState::Blocked
                || !r.evidence.starts_with(crate::mergequeue::HANDED_TO_PROJECT)
                || crate::mergequeue::root_session(store, &r.session_id) != me
                || !told_once(&mut self.gate_told, store, &me, format!("handed:{}", r.id))
            {
                continue;
            }
            out.push(crate::mergequeue::notices::handed_over(&r));
        }
        out
    }

    /// **The reviews this daemon's queues claimed, served by THIS session's runner** — the
    /// review queue's session (`sessions::gatekeeper::session`) is the host; every other session
    /// serves nothing. The body is [`crate::mergequeue::review::serve`]: start a reviewer for each
    /// claimed row nobody is on (placed per decision 3), write the verdict it handed in bound to
    /// the head it judged (decision 6), count an attempt that reached none, and kill a reviewer
    /// whose row moved on without it (a deadline, a person). Answers how many reviewers started.
    pub fn serve_reviews(&mut self) -> usize {
        if !self.is_the_queue() {
            return 0;
        }
        let Some(store) = self.store.as_ref() else {
            return 0;
        };
        let me = crate::mergequeue::lease::this_daemon().owner.clone();
        let placement = crate::sessions::gatekeeper::reviewer_placement(
            self.cfg.reviewers == crate::config::Reviewers::Vm,
            letibot_tools::firecode::firecode_available(),
        );
        let registry = self.session_registry.clone();
        let runner = self.subagents.clone();
        let deliver = move |to: &str, text: &str| {
            letibot_tools::builtins::task::TaskRunner::send(&*runner, to, text)
        };
        let host = self.hub.session_id();
        let pool = crate::mergequeue::review::Pool {
            store,
            runner: &*self.subagents,
            desk: &self.reviews,
            host: &host,
            me: &me,
            placement,
            events: &{
                let registry = registry.clone();
                move |e| {
                    registry.broadcast(e);
                }
            },
            warn: &|repository: &str, e| {
                crate::mergequeue::publish_for_repository(
                    &registry,
                    self.cfg.store.as_deref(),
                    repository,
                    e,
                );
            },
            deliver: &deliver,
            // **The queue's own liveness door**, asked of a refusal whose author and root are
            // both dead: the session that takes it has to be one this daemon holds now.
            live: &|id: &str| crate::mergequeue::session_is_live(&registry, id),
            settings: &crate::mergequeue::settings::for_repository,
            // **The reviewers' declared skills**, from this daemon's project file.
            reviewer_skills: &self.cfg.reviewer_skills,
            clones: self.cfg.review_clones.clone().unwrap_or_else(|| {
                std::env::temp_dir().join(crate::mergequeue::review::CLONES_DIR)
            }),
            slots: &|| crate::vm_reap::slot_holders(store, self.session_registry.instance()),
        };
        let served = match crate::mergequeue::review::serve(&pool) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  review queue: the reviewers could not be served: {e}");
                return 0;
            }
        };
        if served.moved {
            self.fill_queue_board();
        }
        served.started
    }

    /// **Whether this session speaks in the box's room** — the daemon's own question, and the one
    /// the room pass's clock asks before it makes itself a reason to wake.
    pub fn has_room(&self) -> bool {
        self.room.is_some()
    }

    /// **Whose subagent this session is, as the open settled it** — resolved once by
    /// `Registry::parent_for` (the caller's word, then the registry's entry, then the session's own
    /// row), so a child whose parent was settled here stays a child for the life of the process and
    /// the room's door, the settings row and the store row cannot disagree about it.
    pub fn parent_session(&self) -> Option<&str> {
        self.cfg.parent_session_id.as_deref()
    }

    /// **The room's reminders, onto this session's own board** — one row per item it holds, and
    /// the row the idle check carries so a holder is reminded without a model call.
    ///
    /// The operator's ask is `docs/room.md`'s *Still owed* item 3: *"just like with todo top level
    /// session get reminds (with ability to mark waits_on) so they dont occupy it for ever"*. The
    /// room already answers the question (`room reminders`); this is the join.
    ///
    /// **A subagent returns 0 before anything else, and it is the second of two refusals rather
    /// than the only one.** [`crate::room::link`] gives a session with a `parent_session_id` no
    /// connection at all, so `self.room` is `None` for a subagent already — but the room's own rule
    /// is *top-level sessions only*, and a daemon that answered it in one place would be one
    /// refactor away from a subagent with a room row it does not hold.
    ///
    /// **A room that does not answer clears the rows.** The record of a claim is the room's, and a
    /// room that has stopped answering has no record — a coordinator restart empties it, which is
    /// the decision `docs/room.md` states (*"a restart begins with no items at all"*). Leaving a
    /// `you hold …` row on a board after the room has gone would be the one reading that is false
    /// and that nothing would ever correct.
    ///
    /// Returns how many rows the session holds now. It is called on the daemon's idle tick (see
    /// [`crate::room::remind::ROOM_PASS_EVERY`]) and is a no-op that costs one socket frame when
    /// the answer has not moved.
    pub fn fill_room_board(&mut self) -> usize {
        // **The room's rule, said here as well as at the door.** See this function's own doc.
        if self.cfg.parent_session_id.is_some() {
            return 0;
        }
        let Some(room) = self.room.clone() else {
            return 0;
        };
        let me = self.hub.session_id();
        let rows = match room.reminders() {
            Ok(all) => crate::room::remind::rows(&me, &all),
            Err(_) => Vec::new(),
        };
        let n = self.todos.set_room_rows(rows);
        self.flush_todos_or_say("the room's reminders");
        n
    }

    /// Whether this harness is the review queue's own session — the one
    /// [`crate::sessions::gatekeeper::session`] names for this daemon's workspace, whose board is
    /// the queue.
    fn is_the_queue(&self) -> bool {
        self.hub.session_id() == crate::sessions::gatekeeper::session(&self.cfg.workspace)
    }

    /// **The review queue, onto this session's board** — one row per review this daemon's queues
    /// serve that is waiting for a reviewer or under one, the rows a child is on marked with it.
    /// A VIEW, replaced whole from the store; the states stay in the store.
    pub fn fill_queue_board(&mut self) -> usize {
        let Some(store) = self.store.as_ref() else {
            return 0;
        };
        let me = crate::mergequeue::lease::this_daemon().owner.clone();
        let open = self.reviews.open(&self.hub.session_id());
        let rows: Vec<letibot_tokencore::store::TodoItem> =
            crate::sessions::gatekeeper::waiting(store, &me)
                .iter()
                .map(|r| {
                    let child = open
                        .iter()
                        .find(|o| o.entry_id == r.id)
                        .map(|o| o.child.as_str());
                    crate::sessions::gatekeeper::row(r, child)
                })
                .collect();
        let n = self.todos.set_queue(rows);
        self.flush_todos_or_say("the review queue's rows");
        n
    }

    #[cfg(test)]
    pub(crate) fn set_subagents_for_test(
        &mut self,
        runner: Arc<dyn letibot_tools::builtins::task::TaskRunner>,
    ) {
        self.subagents = runner;
    }

    fn queue_verb(
        &self,
        what: &str,
        act: impl FnOnce(&Store, &dyn Fn(SessionEvent)) -> Result<String, String>,
    ) -> Result<String, String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| format!("this daemon has no store, so there are no queues to {what}"))?;
        let registry = self.session_registry.clone();
        act(store, &move |e| {
            registry.broadcast(e);
        })
    }

    /// **The repository a `/queue` verb may act in** (R92 item 5) — `None` is every repository.
    /// [`QueueScope::Here`](crate::slash::QueueScope::Here) is this session's own, and a session
    /// of no repository is told so with the line that reaches every repository.
    pub fn queue_scope(&self, scope: &crate::slash::QueueScope) -> Result<Option<String>, String> {
        crate::mergequeue::verbs::scope_repository(&self.cfg.workspace, scope)
    }

    /// **[`Harness::queue_scope`] for a session of several repositories** — see
    /// [`crate::mergequeue::verbs::scope_repositories`].
    pub fn queue_scopes(
        &self,
        scope: &crate::slash::QueueScope,
    ) -> Result<Vec<Option<String>>, String> {
        crate::mergequeue::verbs::scope_repositories(&self.cfg.workspace, scope)
    }

    /// **Which of `scopes` holds the row `id`** — the first when there is one scope (the verb
    /// refuses a row outside it by name), else the one whose repository the row is of.
    pub fn queue_scope_of_row(&self, scopes: &[Option<String>], id: &str) -> Option<String> {
        if scopes.len() == 1 {
            return scopes[0].clone();
        }
        let its = self
            .store
            .as_ref()
            .and_then(|s| crate::mergequeue::verbs::row_repository(s, id));
        match its {
            Some(r) if scopes.iter().any(|s| s.as_deref() == Some(r.as_str())) => Some(r),
            // A row of none of them: the first scope, whose verb refuses it naming its repository.
            _ => scopes.first().cloned().flatten(),
        }
    }

    /// `/queue restart ID` — a parked review back to waiting, a parked landing back in line.
    pub fn restart_review(
        &self,
        entry_id: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        self.queue_verb("restart anything in", |s, ev| {
            crate::mergequeue::verbs::in_scope(s, entry_id, repository)?;
            crate::mergequeue::verbs::restart(s, entry_id, &me, ev)
        })
    }

    /// `/queue approve ID` — the operator's approval over the reviewer (decision 5).
    pub fn approve_review(
        &self,
        entry_id: &str,
        by: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        self.queue_verb("approve anything in", |s, ev| {
            crate::mergequeue::verbs::in_scope(s, entry_id, repository)?;
            crate::mergequeue::verbs::approve(s, entry_id, by, ev)
        })
    }

    /// `/queue veto ID WHY` — the operator's rejection, delivered to the child that did the work.
    pub fn veto_review(
        &self,
        entry_id: &str,
        why: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        let runner = self.subagents.clone();
        self.queue_verb("veto anything in", |s, ev| {
            crate::mergequeue::verbs::in_scope(s, entry_id, repository)?;
            crate::mergequeue::verbs::veto(
                s,
                entry_id,
                why,
                &me,
                &|to, text| letibot_tools::builtins::task::TaskRunner::send(&*runner, to, text),
                ev,
            )
        })
    }

    /// `/queue rm ID` — withdrawn, with the history kept.
    pub fn remove_merge_entry(
        &self,
        entry_id: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        self.queue_verb("remove anything from", |s, ev| {
            crate::mergequeue::verbs::in_scope(s, entry_id, repository)?;
            crate::mergequeue::verbs::remove(s, entry_id, &me, ev)
        })
    }

    /// `/queue reset [ID]` — every parked row of `repository` restarted, or the one named.
    pub fn reset_reviews(
        &self,
        entry: Option<&str>,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        self.queue_verb("reset", |s, ev| {
            if let Some(id) = entry {
                crate::mergequeue::verbs::in_scope(s, id, repository)?;
            }
            crate::mergequeue::verbs::reset(s, entry, repository, &me, ev)
        })
    }

    /// `/queue clean [ID]` — every parked row of `repository` withdrawn, or the one named.
    pub fn clean_entries(
        &self,
        entry: Option<&str>,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        self.queue_verb("clean", |s, ev| {
            if let Some(id) = entry {
                crate::mergequeue::verbs::in_scope(s, id, repository)?;
            }
            crate::mergequeue::verbs::clean(s, entry, repository, &me, ev)
        })
    }

    /// **The repository this session's queue verbs are about, and its target** — the one its
    /// workspace means (`mergequeue::repo_of_workspace`), keyed the way the queues key it, unless
    /// `named` is a path; and `[queue] target` (else `main`/`master`) when no target is named.
    pub fn queue_repository(
        &self,
        named: Option<&str>,
        target: Option<&str>,
    ) -> Result<(String, String), String> {
        crate::mergequeue::desk::resolve(&self.cfg.workspace, named, target)
    }

    /// `/queue regate [/REPO] [TARGET]` and the `queue` tool's `regate` — the target's tip gated
    /// again even when it was already judged (R76 item 3). `who` goes on the move.
    pub fn regate_target(
        &self,
        repository: Option<&str>,
        target: Option<&str>,
        who: &str,
    ) -> Result<String, String> {
        let (repo, target) = self.queue_repository(repository, target)?;
        self.queue_verb("re-gate anything in", |s, ev| {
            crate::mergequeue::verbs::regate(s, &repo, &target, who, ev)
        })
    }

    /// **Something fired while nothing was running.** T24's wake, from the worker —
    /// and, since R7, **the same door a background job's completion comes through.**
    ///
    /// The daemon calls this when [`letibot_sessionlog::registry::Work::Woken`]
    /// names this session. Every monitor that settled since the cursor, and every
    /// background job the watcher has queued a completion for, becomes one user item
    /// and the loop runs — so a condition that happened between turns is acted on,
    /// rather than sitting in `job_list` until the model happens to ask.
    ///
    /// `Ok(None)` means nothing had settled after all: a wake that raced a mid-turn
    /// pickup by [`HubSteering`], or a job settlement this harness had already turned
    /// into a turn, or a wake with no completion behind it for any other reason. That
    /// is a real outcome and not a failure, and returning `None` rather than running
    /// an empty turn is what stops a spurious wake from costing a generation.
    ///
    /// It returns `Ok(None)` even when there are no monitors: a session that can
    /// background a job and watches no condition still has completions to deliver,
    /// and the early return that used to sit on `self.monitors` is what would have
    /// made R7 silently monitor-only.
    pub fn wake(&mut self) -> Result<Option<Reply>, HarnessError> {
        // **The merge queue's reviews first**, and they are never a turn of this session: a
        // gatekeeper started, or a verdict written, is the queue's business. See
        // [`Harness::serve_reviews`]. A wake is one of the three doors into it and not the only
        // one any more: a session inside a turn is served at its round boundary, and a session
        // this daemon holds is served by the daemon's own clock whatever its turn state.
        let _ = self.serve_reviews();
        // **R92.24: a wake on a workspace that has gone** — a worktree retired under a live
        // session. Said on the hub once per harness, and to the model when this wake has
        // something to say anyway (a wake for this alone would be a turn whose every tool call
        // fails).
        let gone = !self.cfg.workspace.is_dir();
        if gone && !self.workspace_gone_said {
            self.workspace_gone_said = true;
            self.hub.publish(SessionEvent::Warning {
                code: WORKSPACE_GONE.into(),
                detail: workspace_gone(&self.cfg.workspace),
                compaction: None,
            });
        }
        // **Two kinds of thing arrive between turns, and they share one turn.**
        //
        // A monitor that fired, and a background job that ended. Both are the machine's
        // reading of the world with nobody having asked, both must reach the model
        // unprompted, and neither may jump a person who is waiting: that ordering is
        // `Bell::next_any`'s — commands drain before wakes — and it holds for both
        // because both arrive as `Work::Woken`.
        // **And the queue's entries that wait for a merge gate**, said to this session's model —
        // the operator: *"let main project agent manage it"*.
        // **A wake is where a monitor's firing (or expiry) is seen**, so the store's record of
        // what is watching is brought up to date here (`crate::restart`).
        self.sync_monitor_records();
        // **Every sentence below goes through the `notice` hook** (`docs/plugins-plan.md` §4.3):
        // one kind per source, because a layer that patches the wording of a firing is not thereby
        // the answerer for the queue's verdicts. It is applied per source and not to the joined
        // text, so the facts a layer is handed are the ones its sentence was composed from.
        let queue = self.queue_notices();
        let mut notices = self.noticed_all(
            letibot_plugin::notice::MERGE_QUEUE,
            queue,
            serde_json::json!({}),
        );
        // **The room's bells** (the room's slice 2). A room event about this session became a
        // sentence on the connection's own thread — `crate::room::wake` — because the thread that
        // hears a bell is not the thread that runs a turn. This is where it reaches the model, as
        // one notice per event, through the same `notice` hook as every other sentence the harness
        // composes: a layer may reword a room notice and may not delete it (it is a fact the model
        // is owed — somebody else's claim on a resource it also wants).
        let room = crate::room::wake::take(&self.cfg.session_id);
        notices.extend(self.noticed_all(letibot_plugin::notice::ROOM, room, serde_json::json!({})));
        // **And the children a restart brought back** — the operator's *"we should bring
        // subagents back … and restart automatically"*: the model is told, in one line each,
        // which came back and which could not. Counted with the queue's notices: both are facts
        // the daemon found in the store rather than settlements that arrived.
        let restart_taken = self.restart_notices.clone();
        let restarts = std::mem::take(&mut self.restart_notices);
        notices.extend(self.noticed_all(
            letibot_plugin::notice::RESEAT,
            restarts,
            serde_json::json!({}),
        ));
        // **And the entries the queue has handed to this session**, which is the fallback: no
        // reviewer could judge them, and the project merges such a branch through its own main
        // session (`[queue] merge_via_session`), so the project that owns the branch is told — and
        // this is the telling. NOT a refused review: a refusal goes to the child that made the
        // branch through `mergequeue::review::deliver_refusal`, and one that cannot be delivered
        // is raised as `refusal_undelivered`, never handed over here. Second, and
        // after the gate notice, because an entry can be in only one of the two states: the first
        // is *the repository has no gate*, the second is *the queue has given the entry back to
        // you*, and the one that matters to the model reading them is the one that is true.
        let handover = self.handover_notices();
        notices.extend(self.noticed_all(
            letibot_plugin::notice::MERGE_QUEUE,
            handover,
            serde_json::json!({}),
        ));
        // **And a target emergency, or a row parked for a person** (R76 item 1) — the one the
        // queue used to say only on the hubs.
        let emergency = self.queue_emergency_notices();
        notices.extend(self.noticed_all(
            letibot_plugin::notice::MERGE_QUEUE,
            emergency,
            serde_json::json!({}),
        ));
        let queue_n = notices.len();
        let cursor_before = self.monitor_cursor.load(Ordering::SeqCst);
        if let Some(monitors) = self.monitors.clone() {
            let since = self.monitor_cursor.load(Ordering::SeqCst);
            let settled = monitors.settled_count();
            if settled > since {
                let fired: Vec<_> = monitors.firings().into_iter().skip(since).collect();
                self.monitor_cursor.store(settled, Ordering::SeqCst);
                // The monitors pane's table moved with it: a fire, an expiry or a retire.
                self.publish_jobs();
                if !fired.is_empty() {
                    // Immediate by R79's rules, and recorded as such.
                    if let Some(d) = self.deliveries() {
                        d.log_firings(&fired);
                    }
                    if let Some(text) = monitor_notice(&fired) {
                        let facts = monitor_facts(&fired);
                        notices.extend(self.noticed(letibot_plugin::notice::MONITOR, &text, facts));
                    }
                }
            }
        }
        // **The hop that did not exist (R7).** `JobSettled` reaches every head; this is
        // what reaches the model, and taking it here is what stops it arriving twice.
        //
        // **And it is THIS session's queue that is drained, which is the routing fix of
        // 2026-10-05.** The queue is per session: a job a subagent backgrounded is queued
        // on the subagent's set and drained by the subagent, so the notice says *you
        // backgrounded* to the one session that did. Sharing the tree's queue (R58) is
        // what put a subagent's `sleep 25` in the main session's conversation, where the
        // handle was not in `job_list` and `job_output` could not open it. The one thing
        // that still arrives here from below is a settlement whose own session stopped
        // before it could be drained ([`JobWatchers::stop`]) — and `completion_notices`
        // is given this session's id so that it is not read as this session's own.
        //
        // **R79: between turns nothing is held** — a wake runs no turn (`Ok(None)` below) only
        // when there is truly nothing to say. Holding is for a RUNNING turn; see `crate::delivery`
        // and the operator's ruling quoted on `Deliveries::between_turns`.
        let monitor_n = notices.len() - queue_n;
        let deliveries = self.deliveries();
        // **A gatekeeper's settlement is the review's, not this session's news.** The verdict
        // went onto the queue's row above; telling this session's model that "a subagent you
        // started has finished" would be a turn about work that is already recorded and already
        // gone from the board. It holds for the QUEUE's session too, and for the same reason — the
        // pass above ran first and the row is answered (see the field's own doc).
        let gatekeepers = &self.gatekeepers;
        // The sentences, and which kind gets which — [`completion_notices`], which is where that
        // decision lives so that it can be asserted without a live session.
        //
        // **And nothing stays held between turns** (the operator, 2026-10-11: *"jobs were ran for
        // a reason and held for a reason. once turn ends no reason to hold"*): a result that
        // arrived while the session was idle is said here as an ordinary completion, and the batch
        // held while the last turn ran is this wake's own content — the turn-end ring
        // (`JobWatchers::turn_ended`) is what brought the worker here for it.
        //
        // **Not marked read yet (R91.11).** `between_turns` marked them delivered — in the store,
        // for a subagent — and emptied the held set before `submit_item` had put a word in front
        // of the model; a turn that then failed to open lost them. They are marked once the turn
        // has opened, and held again if it does not.
        let between = deliveries.as_ref().map(|d| {
            d.between_turns_unread(
                |c| gatekeepers.contains(&c.job),
                crate::delivery::Rule::TurnEnd,
            )
        });
        let mut held_batch = None;
        let mut said: Vec<JobCompletion> = Vec::new();
        if let Some((b, told)) = between {
            let now = self.noticed_all(letibot_plugin::notice::JOB, b.now, serde_json::json!({}));
            notices.extend(now);
            held_batch = b.batch;
            said = told;
        }
        // **And the rows whose condition just became met.** The same door, because a condition
        // firing is the same kind of fact as a job ending: nothing asked, and the model has to be
        // told. What the line carries is BOTH halves — the row's own words and what the world says
        // about its handle — because either alone is unusable: an intent with no fate is a sentence
        // the model cannot act on, and a fate with no intent is the settlement notice it already
        // has.
        let result_n = notices.len() - queue_n - monitor_n;
        let due = self.due_todo_rows();
        let fired: Vec<String> = due.iter().map(|(_, handle, _)| handle.clone()).collect();
        if !due.is_empty() {
            let lines: Vec<String> = due
                .iter()
                .map(|(text, handle, fate)| format!("  - {text} — {}", fate_line(handle, fate)))
                .collect();
            // R93.C9: "{n} row you attached a condition to is due" was wrong for n > 1.
            let due_line = format!("[todo] Due (its condition is met):\n{}", lines.join("\n"));
            notices.extend(self.noticed(
                letibot_plugin::notice::TODO,
                &due_line,
                serde_json::json!({"rows": due.len()}),
            ));
        }
        let causes = [
            queue_n,
            monitor_n,
            result_n,
            due.len(),
            usize::from(held_batch.is_some()),
        ];
        // **The held batch last**, after everything said now.
        let held: Vec<String> = match held_batch.as_deref() {
            Some(text) => self
                .noticed(letibot_plugin::notice::JOB, text, serde_json::json!({}))
                .into_iter()
                .collect(),
            None => Vec::new(),
        };
        notices.extend(held);
        // **A wake with nothing to say runs no turn**, so nothing is owed for what the take
        // removed — and this has to be cleared BEFORE the publish below, or the value a reader
        // holds says delivery about a turn that is not coming.
        if notices.is_empty()
            && let Some(w) = &self.job_watch
        {
            w.turn_began();
        }
        // **What this session is waiting on has moved** (`PROTOCOL_VERSION` 52): the monitor cursor
        // above took the firings, and `between_turns_unread` took the queue and the held set — none
        // of which publishes an event of its own. Said before the early return below, because the
        // move that matters most is the one that empties the list: a wake with nothing left to say
        // is a session that has just gone quiet.
        self.publish_activity();
        self.meter_wake(causes, !notices.is_empty());
        if notices.is_empty() {
            return Ok(None);
        }
        if !self.cfg.workspace.is_dir() {
            let line = format!("[workspace] {}.", workspace_gone(&self.cfg.workspace));
            if let Some(line) = self.noticed(
                letibot_plugin::notice::WORKSPACE,
                &line,
                serde_json::json!({}),
            ) {
                notices.insert(0, line);
            }
        }
        let text = notices.join("\n\n");
        // **The wall is checked BEFORE the send — `nag_turn`'s pre-flight, the
        // same function.** MEASURED 2026-10-09: the FIRST of the thirty
        // consecutive provider refusals on `s-1789919514688401228` was
        // attributed to `monitor` — this door — so the insurance is not
        // theoretical. As with the nag, this is not the load-bearing fix: a
        // refused wake is classified as the wall and recovered by the turn tail
        // (`Sessions::wake` calls it), and this pre-flight is what spares the
        // refused round-trip when the daemon's own ledger already says the next
        // turn will not fit.
        self.compact_if_at_the_wall();
        // The harness talking to itself, not the operator. A firing — or a job's end —
        // must never be able to authorise the action it reports on.
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        let mut opened = false;
        let out = self.submit_items_then(
            vec![TranscriptItem::User {
                // The same speaker the trail records, one line up: this is the session talking
                // to itself, and a head must not draw it as the operator's words (R42).
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text }],
            }],
            |_| opened = true,
        );
        // **Told only once the turn has opened (R91.11).** Opened means the notice is in the
        // transcript and in the store, so the model reads it whatever its rounds come to. A turn
        // that never opened put nothing in front of anybody: the results go back to the held
        // set, the restart notices back to the harness, and the monitor cursor back to where it
        // was, so the next wake says all of it.
        if opened {
            if let Some(d) = &deliveries {
                d.read(&said);
            }
        } else {
            if let Some(d) = &deliveries {
                d.put_back(said);
            }
            let mut back = restart_taken;
            back.append(&mut self.restart_notices);
            self.restart_notices = back;
            self.monitor_cursor.store(cursor_before, Ordering::SeqCst);
        }
        out.map(Some)
            // ...and the condition is spent only once the model HAS been told. Consuming before the
            // submit would let a turn that failed to start silently eat the firing, and **a firing
            // nobody hears is the failure this whole mechanism exists to rule out**, where a firing
            // reported twice is merely dull. So the order here is the requirement and not tidiness.
            .inspect(|_out| {
                if !fired.is_empty() {
                    self.todos.consume_conditions(&fired);
                    // Unwritten, the condition is still pending in the store and fires again after
                    // a restart — which is why this is said on the hub and not only on stderr.
                    self.flush_todos_or_say("a fired condition");
                }
            })
    }

    /// **A subagent brought back after a daemon restart carries on** — the operator, 2026-10-12:
    /// *"we should bring subagents back - i cant afford to wait till they finish. we should record
    /// them (session persist anyway) and restart automatically"*.
    ///
    /// The session was reopened from the store under its own id, so the conversation is the
    /// child's own; what it lacks is the turn that was in flight. This is that turn, opened by
    /// `notice` (`crate::subagent_record::child_notice`) in the harness's own voice — the shape of
    /// [`Harness::continue_after_wall`]: `Speaker::Agent`, never an authorisation, never readable
    /// as the operator's words. A child of THIS child that was brought back too is said in the
    /// same turn ([`Harness::restore_after_restart`]), and so is anything held for it.
    pub fn continue_after_restart(&mut self, notice: &str) -> Result<Reply, HarnessError> {
        let mut parts = vec![notice.to_string()];
        parts.append(&mut self.restart_notices);
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            parts.push(meanwhile);
        }
        let text = parts.join("\n\n");
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
    }

    /// **A settlement this daemon queued for a session that was not open is not lost.**
    ///
    /// # The hole this closes
    ///
    /// [`crate::jobwatch::JobWatchers`] is built **per open**, so a settlement that lands while
    /// its session is not open has nowhere to go: the queue it is pushed into belongs to a
    /// harness nobody will drain again, and the ring that would have started the turn names a
    /// session `Sessions::wake` answers `Gone` for (`sessions::wake_route`). Both die with the
    /// harness. What survives is the **row**, because it is written where the settlement is first
    /// known rather than where it is read: `subagent_run` by the child's own thread
    /// ([`crate::subagent_record::SubagentRecord::settled`]) and `job` by
    /// [`crate::jobwatch::JobRecorder::record`] — each with `delivered = 0`, which is that
    /// column's whole meaning (*the model has not been told*).
    ///
    /// This is the door that reads it, at the one moment it is true: the session has just been
    /// opened, so its watcher set is new and empty and nothing in this process is holding the
    /// settlement. Queued as an ordinary [`JobCompletion`], it is worded by the same
    /// `completion_notices` a live settlement is — *a subagent you started has finished: …* — so
    /// a settlement that outlived its session is not a second kind of notice. The open's own
    /// ring (`Sessions::put_a_restored_session_to_work`) is what makes the turn.
    ///
    /// # Only this daemon's own, and that is the other half rather than a narrowing
    ///
    /// `instance` is the daemon that last owned the row. A row a **gone** daemon wrote is
    /// [`Harness::restore_after_restart`]'s (`runs`/`settled` below, which filter on
    /// `instance != me`), and it is worded as a restart because that is what it was. A row
    /// **this** daemon wrote and never delivered is what the open of a session it had closed
    /// finds, and it is worded as a completion because nothing restarted. The two filters are
    /// disjoint, so no settlement is delivered twice.
    ///
    /// # What it cannot do, said rather than implied
    ///
    /// A job's output is gone by now — `job_output` reads the process host's own capture, and
    /// the process is long reaped — so the notice names the job and says how it ended, which is
    /// what `job_output`'s absence leaves true. A child's answer IS in the row (`outcome`), and
    /// the notice carries its first line exactly as a live settlement does.
    ///
    /// # What it returns
    ///
    /// How many settlements it queued. A queued settlement is a notice the next wake says, and a
    /// session whose ONLY news is one of them is a session that still owes work — which is
    /// [`Harness::restore_after_restart`]'s decision about the restart sentence's door, so the
    /// count has to leave this function rather than be recomputed there.
    fn deliver_settlements_left_undelivered(&self, store: &Store, me: &str, sid: &str) -> usize {
        let Some(watch) = self.job_watch.clone() else {
            return 0;
        };
        let mut queued = 0usize;
        // **The children first** — this row's own subject, and the one the operator went
        // looking for: a `task` child that answered into a session nobody was holding.
        for run in store.subagent_runs_of(sid).unwrap_or_default() {
            if run.instance != me
                || run.delivered
                || run.parent_id != sid
                || crate::subagent_record::is_helper(&run)
                || !matches!(run.state.as_str(), "done" | "failed")
            {
                continue;
            }
            watch.queue(JobCompletion {
                memory: None,
                kind: BackgroundKind::Subagent,
                // The session that started it, which is this one — the sentence is *a subagent
                // YOU started*, and it is true: the row's `parent_id` is this session.
                owner: sid.to_string(),
                job: run.child_id.clone(),
                command: String::new(),
                slug: Some(crate::subagent_record::recorded_slug(&run)),
                state: run.state.clone(),
                produced: 0,
                elapsed_ms: 0,
                detail: crate::jobwatch::first_line(run.outcome.as_deref().unwrap_or_default()),
                arrived: std::time::SystemTime::now(),
            });
            queued += 1;
        }
        // **And the jobs**, whose rows the recorder writes for the same reason and with the same
        // `delivered = 0`. A child's handle has a `running` job row too (the sink writes one for
        // every `Backgrounded`, before it knows which kind it is), so it is skipped here the way
        // `restore_after_restart` skips it.
        for job in store.undelivered_jobs(sid).unwrap_or_default() {
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let Some(ctx) = store.job_context(sid, &job.handle).ok().flatten() else {
                // No context means no daemon is named, so there is nothing that says this row is
                // this daemon's — and a row nobody can attribute is `restore_after_restart`'s
                // (a relic) rather than a settlement this door dropped.
                continue;
            };
            if ctx.instance != me {
                continue;
            }
            watch.queue(JobCompletion {
                memory: None,
                kind: BackgroundKind::Job,
                owner: sid.to_string(),
                job: job.handle.clone(),
                command: job.command.clone(),
                slug: ctx.slug.clone(),
                state: job.state.clone(),
                produced: job.produced,
                elapsed_ms: job.elapsed_ms,
                detail: String::new(),
                arrived: std::time::SystemTime::now(),
            });
            queued += 1;
        }
        if queued > 0 {
            eprintln!(
                "  {sid} · {queued} settlement(s) this daemon queued while the session was not \
                 open are said at this open"
            );
        }
        queued
    }

    /// **What this session was running when its daemon stopped — restored, or reminded** — called
    /// once, when the session is opened (`Sessions::harness_as`, `Sessions::open_first_with_seat`,
    /// and a resumed child's own `run_to_completion`). See `crate::restart` for the whole of it.
    ///
    /// The operator, 2026-10-12: *"to make restarts as much painless as possible - by default we
    /// restore agents, monitors, and remind the model about jobs. Now, if some threshold passed -
    /// say more than 1 hours after the stop, we start to simply remind the model stating that time
    /// passed and this is what you wre running and how to restart that"* — and *"this is the case
    /// for when harnessd stopped of course"*: only records a daemon that is GONE wrote are read, so
    /// a session opened by the daemon that holds its records (an attach after a long idle, a
    /// `/resume`) does nothing here.
    ///
    /// Everything it decides is one notice: queued for this session's next turn
    /// ([`Harness::restart_notices`] — the daemon rings the session right after the open, so that
    /// turn is now), published on the hub as `restart_restored` or `restart_reminded` for the
    /// operator, and printed on the daemon's log. Every child resumed or re-delivered is watched by
    /// this session's own watcher set, so its completion arrives exactly as a fresh child's does.
    ///
    /// `None` when there was nothing to do — the ordinary open.
    pub fn restore_after_restart(&mut self) -> Option<String> {
        use letibot_tools::builtins::task::{RestartFate, RestoreMode};
        let path = self.cfg.store.clone()?;
        let store = Store::open(&path).ok()?;
        let me = self.session_registry.instance().to_string();
        let back_ms = self.session_registry.started_ms();
        // **When this session is being opened** — the clock the restore threshold is applied to
        // (R91.8): the operator's rule is *"more than 1 hours after the stop"*, and a session
        // opened hours after the daemon came back is exactly the case the threshold exists for.
        let opened_ms = letibot_sessionlog::event::now_ms();
        let sid = self.cfg.session_id.clone();
        // **Before anything about a restart, the settlements THIS daemon dropped** — see
        // [`Harness::deliver_settlements_left_undelivered`]. It lives here because this is the
        // door every open passes through (`Sessions::harness_as`, `Sessions::open_first_with_seat`,
        // a resumed child's `run_to_completion`) and it is the door whose own ring, a few lines
        // later in `Sessions`, is what turns a queued settlement into a turn. The count is kept
        // because a settlement nobody has been told about is one of the things that makes an
        // otherwise empty restart a TURN ([`crate::restart::Owed`]).
        let settlements = self.deliver_settlements_left_undelivered(&store, &me, &sid);

        // **What a gone daemon left**, and which daemon that was. Read before anything is
        // touched, so the down time is decided once for the whole session.
        let runs: Vec<_> = store
            .subagent_runs_of(&sid)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                r.instance != me
                    // A tool's helper is never restored or re-delivered (`decide`); a running one
                    // is only closed, which needs no notice.
                    && !crate::subagent_record::is_helper(r)
                    && (r.state == "running"
                        || (matches!(r.state.as_str(), "done" | "failed") && !r.delivered))
            })
            .collect();
        // Helpers caught mid-task by the stop are closed quietly by the runner's pass below even
        // when nothing else in this session needs a notice.
        let retire_helpers = store
            .subagent_runs_of(&sid)
            .unwrap_or_default()
            .iter()
            .any(|r| {
                r.instance != me && r.state == "running" && crate::subagent_record::is_helper(r)
            });
        let monitors: Vec<_> = store
            .monitors_of(&sid)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.live && m.instance != me)
            .collect();
        let mut jobs = Vec::new();
        // **A job row with no context is a relic of a build before `job_context`**, not a job
        // that was running at THIS stop: it names no daemon, so it cannot say when anything
        // stopped, and reminding the model of it as running is false. pg-noop, 2026-10-15: four
        // such rows (j900, j610, j624, j2437, from builds long gone) were the session's only
        // records, `Stopped::latest` over no daemon at all said the stop was unknown, and a
        // daemon down a couple of seconds told the model *"when the previous one stopped is
        // unknown … NOTHING was restored automatically"* over four jobs nobody was running.
        // They are closed quietly below, the way a reminded job is, and said to nobody.
        let mut relics = Vec::new();
        for job in store.jobs(&sid).unwrap_or_default() {
            // **A job the old daemon's ORDERLY stop ended is a job that was running at the
            // stop**, and is reminded like one. `Registry::close` ends every run in flight
            // (`end_running`, *"killed by the daemon stopping"*) before the process exits, so
            // after a clean restart no job row reads `running` at all — and the reminder this
            // pass exists to give was never given: pg-noop's `slt-sweep` (j17239), 4,262 of
            // 4,771 files into its measurement, killed at 02:05:55 and mentioned to nobody.
            if job.state != "running" && job.state != crate::restart::STOPPED_WITH_THE_DAEMON {
                continue;
            }
            // A subagent's handle is backgrounded through the same sink and has a `job` row too;
            // it is the child record's business, not a job's.
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let ctx = store.job_context(&sid, &job.handle).ok().flatten();
            if ctx.as_ref().is_some_and(|c| c.instance == me) {
                continue;
            }
            if ctx.is_none() && job.command.is_empty() {
                continue;
            }
            match ctx {
                Some(ctx) => jobs.push((job, ctx)),
                None => relics.push(job),
            }
        }
        // **Jobs that ENDED and were never delivered** (TODO.md R91.5): a result held under R79,
        // or queued, when the daemon stopped. Only a gone daemon's — one this daemon holds is
        // still in its own queue — and only with a context, which is what names the daemon.
        let mut settled = Vec::new();
        for job in store.undelivered_jobs(&sid).unwrap_or_default() {
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let Some(ctx) = store.job_context(&sid, &job.handle).ok().flatten() else {
                continue;
            };
            if ctx.instance == me {
                continue;
            }
            settled.push((job, ctx));
        }
        for job in relics {
            let mut lost = job.clone();
            lost.state = "lost at restart".into();
            let _ = store.put_job(&sid, &lost);
            let _ = store.deliver_job(&sid, &job.handle);
        }
        // **A restart is SAID, whatever the store holds** — the operator, 2026-10-30, after one
        // restart had told the model something and the next had said nothing: *"mb always tell
        // agent about restart? with the current time lol"*. That is a clock and not a courtesy:
        // the model's sense of time went through a stop and a start, and only this daemon knows
        // when. A session that existed before this daemon started was held by a daemon that is
        // gone — which is the whole of what `came_back` asks, and it is asked of the session ROW
        // because a session with nothing running has no other record to ask.
        // **And a daemon actually stopped here.** A session older than this daemon is not by itself
        // a restart: a seeded session, a resumed one, one this daemon opened first — all predate
        // `back_ms` while nothing stopped. What makes it a restart is another daemon's `daemon_life`
        // row carrying a stop, which is a fact about the box rather than an inference from a clock.
        let a_daemon_stopped = store
            .lives()
            .map(|lives| {
                lives
                    .iter()
                    .any(|l| l.stopped_ms.is_some() && l.instance != me)
            })
            .unwrap_or(false);
        // **And only the sessions a person reads.** The review queue's own session is opened by
        // this daemon too, and it is nobody's conversation: the operator saw the restart twice —
        // once for his session and once for the gatekeeper's — and asked for one
        // (*"i dont need two"*). A subagent is nobody's either; it reads what its parent tells it.
        let a_persons_session = !letibot_sessionlog::is_gatekeeper_session(&sid)
            && !matches!(store.session_parent(&sid), Ok(Some(Some(p))) if !p.is_empty());
        // **The session's own row, read once**, because it answers two questions: whether this
        // session existed before this daemon started (so a daemon that is gone held it), and what
        // its last row was — which is the only evidence a session that was mid-turn when the
        // daemon stopped leaves behind ([`crate::restart::mid_turn`]).
        let row = store.session(&sid).ok().flatten();
        let came_back = a_daemon_stopped
            && a_persons_session
            && row
                .as_ref()
                .is_some_and(|s| (s.created_ms.max(0) as u64) < back_ms);
        // **Once per (session, restart)** — `queue_told` is the store's own "said once" table and
        // the key names the daemon, so a session this daemon opens twice says it once, and the
        // NEXT daemon says it again because it is a different restart. Spent on the first open
        // whatever that open says, so an empty reopen cannot say what the first one already did.
        // A store that cannot record it is treated as *not yet said*: a sentence said twice is
        // dull, and one never said is the defect this exists for.
        let already_said = came_back
            && !store
                .tell_once(&format!("restart:{sid}:{me}"), back_ms)
                .unwrap_or(true);
        if runs.is_empty() && monitors.is_empty() && jobs.is_empty() && settled.is_empty() {
            if retire_helpers {
                let _ = self.subagents.resume_recorded(RestoreMode::Remind);
            }
            if !came_back || already_said {
                return None;
            }
            // **What this session still owes decides the door its sentence takes** — the
            // operator, 2026-11-01: *"it is not ok that restart notice arrived with my continue"*.
            // Nothing owed keeps the row (a generation spent on a sentence is still a generation
            // spent); anything owed makes the sentence the content of the turn the work already
            // costs. See [`crate::restart::Owed`].
            let owed = self
                .owed_after_a_restart(row.as_ref().and_then(|s| s.last_item.as_ref()), settlements);
            return self.say_nothing_was_running(&store, back_ms, &owed);
        }

        // **How long the daemon was down**: from the latest stop among the daemons these records
        // came from, to this daemon's start — the fact the notice's first line states. **The
        // threshold is applied to a different interval** (R91.8): stop → this open
        // (`restores(opened_ms, …)`), because a session opened hours after the daemon came back is
        // the case the threshold is for.
        let mut instances: Vec<(String, u64)> = Vec::new();
        for (inst, at) in runs
            .iter()
            .map(|r| (r.instance.clone(), r.updated_ms))
            .chain(monitors.iter().map(|m| (m.instance.clone(), m.updated_ms)))
            .chain(jobs.iter().map(|(_, c)| (c.instance.clone(), c.started_ms)))
            .chain(
                settled
                    .iter()
                    .map(|(_, c)| (c.instance.clone(), c.started_ms)),
            )
        {
            match instances.iter_mut().find(|(i, _)| *i == inst) {
                Some((_, t)) => *t = (*t).max(at),
                None => instances.push((inst, at)),
            }
        }
        // **Every record here names a daemon** (a relic does not, and was closed above), so the
        // stop is UNKNOWN only when one of those daemons kept no life row — no clock, not "down
        // for 0s" — and an unknown down time restores nothing (`restart` module docs).
        let stop = crate::restart::Stopped::latest(
            instances
                .iter()
                .map(|(i, at)| crate::restart::stop_of(&store, i, *at))
                .chain(std::iter::once(latest_box_stop(&store))),
        );
        let restore = stop.restores(opened_ms, self.cfg.restore_within);
        // Whether anything WAS brought back — which decides the notice's first line and its code.
        let mut restored_any = false;
        let mode = if restore {
            RestoreMode::Resume
        } else {
            RestoreMode::Remind
        };

        let mut lines: Vec<String> = Vec::new();
        let mut said: Vec<String> = Vec::new();

        // **Subagents.**
        for c in self.subagents.resume_recorded(mode) {
            let name = format!("`{}` ({})", self.subagents.label_of(&c.handle), c.title);
            match &c.fate {
                RestartFate::Resumed => {
                    restored_any = true;
                    if let Some(w) = &self.job_watch {
                        w.watch(c.handle.clone());
                    }
                    lines.push(crate::subagent_record::parent_resumed_line(&name));
                    said.push(crate::restart::said::resumed(&name));
                }
                RestartFate::Settled => {
                    restored_any = true;
                    // The settlement itself is the notice the watcher queues; a second line about
                    // it would tell the model the same thing twice.
                    if let Some(w) = &self.job_watch {
                        w.watch(c.handle.clone());
                    }
                    said.push(crate::restart::said::delivering(&name));
                }
                RestartFate::NotResumed { why } => {
                    lines.push(crate::subagent_record::parent_not_resumed_line(&name, why));
                    said.push(crate::restart::said::not_resumed(&name, why));
                }
                RestartFate::Reminded { worktree, branch } => {
                    lines.push(crate::subagent_record::parent_reminded_line(
                        &name,
                        &c.handle,
                        worktree.as_deref(),
                        branch.as_deref(),
                    ));
                    said.push(crate::restart::said::subagent_reminded(&name));
                }
            }
        }

        // **Monitors.** One that had already run out its TTL before the stop is closed quietly:
        // it would have ended on its own, restart or none.
        let rt = self.runtime.clone();
        for m in monitors {
            let _ = store.end_monitor(&sid, &m.name);
            if m.declared_ms.saturating_add(m.ttl_ms) <= stop.no_earlier_than_ms() {
                continue;
            }
            if !restore {
                lines.push(crate::restart::monitor_reminded_line(&m));
                said.push(crate::restart::said::monitor_reminded(&m.name));
                continue;
            }
            // **A monitor a named scope owned is the model's to bring back**, however short the
            // stop: the scope was something it set up, the stopped daemon was the only one that
            // knew it, and whether it still matters is a judgement, not a replay.
            if let Some(scope) = crate::restart::named_owner(&m) {
                lines.push(crate::restart::monitor_scoped_line(&m, &scope));
                said.push(crate::restart::said::monitor_scoped(&m.name, &scope));
                continue;
            }
            // **The same call, made again.** It was gated when the model first made it; this
            // re-establishes that declaration rather than making a new one, so it goes through the
            // door with no gate (the operator's own runs use it for the same reason: nobody is
            // left to ask). The recording wrapper writes the new declaration under this daemon.
            let call = ToolCall {
                id: format!("restart-monitor-{}", m.name),
                name: "monitor".into(),
                arguments: m.args_json.clone(),
            };
            let r = rt.invoke_operator("restart", &call, &mut letibot_tools::NullToolSink);
            if r.outcome == letibot_transcript::ToolOutcome::Ok {
                restored_any = true;
                lines.push(crate::restart::monitor_rearmed_line(&m));
                said.push(crate::restart::said::rearmed(&m.name));
            } else {
                let why = r
                    .payload
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('.')
                    .to_string();
                lines.push(crate::restart::monitor_not_rearmed_line(&m, &why));
                said.push(crate::restart::said::not_rearmed(&m.name, &why));
            }
        }

        // **Jobs: reminded, never re-run.**
        let rows = self.todos.snapshot();
        for (job, ctx) in jobs {
            let slug = ctx.slug.clone();
            let waiting = crate::restart::rows_waiting_on(&rows, &job.handle, slug.as_deref());
            let ran = stop.at_ms().map(|at| at.saturating_sub(ctx.started_ms));
            lines.push(crate::restart::job_line(
                &job,
                slug.as_deref(),
                Some(ctx.cwd.as_str()),
                ran,
                &waiting,
            ));
            said.push(crate::restart::said::job(&job.handle, waiting.len()));
            // **Said once**: the row stops reading `running`, so the next open does not remind
            // again and the pane does not draw a job nobody runs.
            let mut lost = job.clone();
            lost.state = "lost at restart".into();
            let _ = store.put_job(&sid, &lost);
            // And said: a `lost at restart` row is an ending, and an undelivered ending is what
            // the next open would announce again.
            let _ = store.deliver_job(&sid, &job.handle);
        }

        // **Jobs that ended untold: their ending, said now** — whatever the down time, the way a
        // settled child's result is delivered past the threshold too: a result is not work to
        // redo, and nothing is re-run. Marked delivered once said, so it is said once.
        for (job, ctx) in settled {
            restored_any |= restore;
            lines.push(crate::restart::settled_job_line(&job, ctx.slug.as_deref()));
            said.push(crate::restart::said::job_settled(&job.handle));
            let _ = store.deliver_job(&sid, &job.handle);
        }

        let outcome = match (restore, restored_any) {
            (true, true) => crate::restart::Outcome::Restored,
            (true, false) => crate::restart::Outcome::NothingRestorable,
            (false, _) => crate::restart::Outcome::Reminded,
        };
        let text = crate::restart::notice(
            &crate::restart::header(&stop, back_ms, opened_ms, self.cfg.restore_within, outcome),
            &lines,
            outcome,
        );
        for line in &said {
            eprintln!("  {sid} · restart: {line}");
        }
        self.hub.publish(SessionEvent::Warning {
            code: outcome.code().into(),
            detail: crate::restart::warning_detail(&text, &said),
            compaction: None,
        });
        self.restart_notices.push(text.clone());
        Some(text)
    }

    /// **A restart with nothing to restore, said anyway** — the operator, 2026-10-30: *"mb always
    /// tell agent about restart? with the current time lol"*. The sentence is
    /// [`crate::restart::nothing_running`]'s; this is the reading behind it and the door it takes.
    ///
    /// # Which daemon stopped, from a session with no work in it
    ///
    /// The stop is dated from the daemons the session's OWN rows name, whatever state those rows
    /// are in: a monitor it retired, a child that finished, the last job it ran. A session that
    /// names nobody — created, never used, nothing run in it — has no record that could date a
    /// stop, and the sentence says `unknown` rather than borrowing the newest stop in the store,
    /// which belongs to whichever daemon on this box stopped last and is a claim about another
    /// folder's process.
    ///
    /// # The door, and which of the two it takes
    ///
    /// **A session that owes nothing** has nothing to continue, so the sentence is filed and the
    /// model reads it when it next reads ([`Harness::say_without_a_turn`]): no wake, no turn. A
    /// restore is the opposite — a resumed child is work to continue — and keeps the turn it has
    /// always had.
    ///
    /// **A session that owes something is the operator's ruling of 2026-11-01**: *"it is not ok
    /// that restart notice arrived with my continue … so i expected that after restart you will
    /// continue"*. The sentence is queued as a restart notice instead, which is the turn the
    /// daemon's own ring at open already costs for the work ([`crate::restart::Owed`], and
    /// [`Harness::owed_after_a_restart`] for what counts). The cost of the whole mechanism is one
    /// model call per restart per session that owed anything; a session that owes nothing keeps
    /// the row and spends none.
    fn say_nothing_was_running(
        &mut self,
        store: &Store,
        back_ms: u64,
        owed: &crate::restart::Owed,
    ) -> Option<String> {
        let sid = self.cfg.session_id.clone();
        let mut instances: Vec<(String, u64)> = Vec::new();
        let mut note =
            |instance: &str, at: u64| match instances.iter_mut().find(|(i, _)| i == instance) {
                Some((_, t)) => *t = (*t).max(at),
                None => instances.push((instance.to_string(), at)),
            };
        for m in store.monitors_of(&sid).unwrap_or_default() {
            note(&m.instance, m.updated_ms);
        }
        for r in store.subagent_runs_of(&sid).unwrap_or_default() {
            note(&r.instance, r.updated_ms);
        }
        // The newest job row is one query, and it is the row a session that ran nothing since
        // its last `bash` names its daemon by.
        if let Some(last) = store.jobs(&sid).unwrap_or_default().last()
            && let Some(ctx) = store.job_context(&sid, &last.handle).ok().flatten()
        {
            note(&ctx.instance, ctx.started_ms);
        }
        let stop = crate::restart::Stopped::latest(
            instances
                .iter()
                .map(|(i, at)| crate::restart::stop_of(store, i, *at))
                .chain(std::iter::once(latest_box_stop(store))),
        );
        // **The same sentence, and one clause when the session owes work** — a model woken to
        // *nothing was running* alone could not know why it is reading it now.
        let text = match owed.any() {
            true => crate::restart::nothing_running_but(&stop, back_ms, owed),
            false => crate::restart::header(
                &stop,
                back_ms,
                letibot_sessionlog::event::now_ms(),
                self.cfg.restore_within,
                crate::restart::Outcome::Nothing,
            ),
        };
        eprintln!("  {sid} · restart: nothing was running — {text}");
        // **One line, not two.** In the quiet case the sentence is written as a transcript row
        // ([`Harness::say_without_a_turn`]), which the person sees where the rest of the
        // conversation is and the model reads at its next turn. In the owed case it is queued as
        // a restart notice, which the wake spends a turn on and the person sees as that turn's
        // opening line. Neither publishes a hub warning: the row drew the restart twice for one
        // stop — the operator: *"i need only one restart notification"* — and the row is the copy
        // that both readers can use.
        if owed.any() {
            self.restart_notices.push(text.clone());
        } else if let Err(e) = self.say_without_a_turn(&text) {
            eprintln!("  {sid} · restart: the notice was not recorded ({e})");
        }
        Some(text)
    }

    /// **What this session still owes**, as [`crate::restart::Owed`] — the four facts that decide
    /// whether an otherwise empty restart is a turn.
    ///
    /// `last` is the session row's last transcript row (read once by
    /// [`Harness::restore_after_restart`]) and `settlements` is what
    /// [`Harness::deliver_settlements_left_undelivered`] queued at this open.
    ///
    /// The board is read twice on purpose: `due_rows` is the rows whose condition the world now
    /// meets ([`Harness::due_todo_rows`], the wake's own predicate, so the two cannot disagree)
    /// and `plan_rows` is every row still owed — the plan the model wrote, which is the one thing
    /// a session with no record of a turn leaves that says it has work left. A `Postponed` or
    /// `Cancelled` row is the operator's act and is not owed; a `Completed` one is answered.
    fn owed_after_a_restart(
        &self,
        last: Option<&TranscriptItem>,
        settlements: usize,
    ) -> crate::restart::Owed {
        let plan_rows = self
            .todos
            .snapshot()
            .iter()
            .filter(|r| matches!(r.status, TodoStatus::Pending | TodoStatus::InProgress))
            .count();
        crate::restart::Owed {
            mid_turn: crate::restart::mid_turn(last),
            settlements,
            due_rows: self.due_todo_rows().len(),
            plan_rows,
        }
    }

    /// **Tell this session's model something without starting a turn** — the door a restart with
    /// nothing owed takes (`crate::restart`), and the two states it has to tell apart.
    ///
    /// * **A turn is running** → the step boundary of THAT turn (`self.injected`, the queue the
    ///   harness's own nudges ride): the model reads the sentence in the turn already open, and
    ///   nothing is preempted.
    /// * **Idle** → a transcript row, which the model reads at its next turn — whenever that is.
    ///   **No wake**: a wake is a model call, and a call spent to say *"the daemon came back at
    ///   21:28"* to a session that owes nothing is a call spent on a sentence. What the line is
    ///   for is closing a gap in the model's sense of time, and that gap is closed whenever it
    ///   next reads.
    ///
    /// **And that is the cost argument's whole reach.** It decides the door for a session with
    /// nothing outstanding; a session that owes work takes the turn it already costs
    /// ([`Harness::say_nothing_was_running`], [`crate::restart::Owed`]), because there the person
    /// is waiting for the session to continue and a row is read when they type — the operator,
    /// 2026-11-01: *"it is not ok that restart notice arrived with my continue"*.
    ///
    /// A row rather than an injection in the idle case, deliberately: an injection is read at a
    /// step boundary and there is no step boundary to wait for, while a row is in the transcript
    /// whether or not a turn ever runs — the same reason [`Harness::note_to_model`] queues rather
    /// than speaks.
    fn say_without_a_turn(&mut self, text: &str) -> Result<(), HarnessError> {
        let Some(text) = self.noticed(letibot_plugin::notice::RESEAT, text, serde_json::json!({}))
        else {
            return Ok(());
        };
        if self.prompt_in_flight.load(Ordering::SeqCst) {
            self.injected
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(text);
            return Ok(());
        }
        self.record_notice(&text)
    }

    /// **A turn is running, as far as this test is concerned** — the one state
    /// [`Harness::say_without_a_turn`] branches on that a test cannot reach by running a turn: a
    /// turn holds the harness for its whole life, so nothing else can call in while one is in
    /// flight. See `tests::restart_notice`.
    #[cfg(test)]
    pub(crate) fn hold_a_turn_open_for_test(&self, open: bool) {
        self.prompt_in_flight.store(open, Ordering::SeqCst);
    }

    /// **Monitors this daemon recorded that are no longer live** are closed in the store, so a
    /// restart re-arms only what was really watching. Called where a firing, a retirement or an
    /// expiry has just been seen (a wake, a turn's end).
    fn sync_monitor_records(&mut self) {
        let (Some(monitors), Some(path)) = (&self.monitors, &self.cfg.store) else {
            return;
        };
        let mut live = monitors.live_names();
        live.sort();
        // **Only when the live set moved** — a wake is frequent and a store open is not free, and
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

    /// **The turn a fallback earned, opened by the notice that says so** — the operator's ruling
    /// of 2026-10-12 (*"when it finally applied the turn didnt continue"*), built as an act.
    ///
    /// `turn_tail::run_turn` is the only caller, and it makes this a whole turn of its own through
    /// the same sequence every door gets: the refusal ended the last one, and this one starts with
    /// a user item in the harness's own voice (`Speaker::Agent`, as a wake's and a plan-check's
    /// notices are) — which is also what makes the work legal for the model that took over, since
    /// a model's first request has to follow a user message.
    ///
    /// **The held batch rides in**, by R79's rule: this is a turn the model takes anyway, so the
    /// results it is owed at a turn start go with it rather than waiting for a prompt.
    pub fn continue_after_fallback(&mut self, notice: String) -> Result<Reply, HarnessError> {
        let mut text = notice;
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        // The wall is checked BEFORE the send, like every other door: a session that inherited a
        // refusal is not more likely to fit, and a first round refused for LENGTH would be read as
        // another provider failure.
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
    }

    /// **The continuation turn a fallback owed, taken — at most once, and never over a stop.**
    ///
    /// `None` is the ordinary case. Two ways to get it back empty, and both are the point:
    ///
    /// * the turn that just ended was itself a fallback continuation — see
    ///   [`crate::turn_tail::run_turn`], which never takes one twice, so a fallback chain cannot
    ///   ping-pong between two names and pay a model change per turn for ever;
    /// * **the operator asked for a stop the failed turn did not take.** An interrupt that
    ///   arrived after that turn's last round sits in the queue with nothing to act on it, and a
    ///   fresh turn started on top of it would either be aborted by it or, worse, ignore their
    ///   stop. Their word ends the work here, and the session stays on the fallback model.
    ///
    /// **It always clears the field**, taken or not: a notice left armed would open a turn nobody
    /// asked for at the end of some later turn.
    pub(crate) fn take_fallback_continuation(&mut self) -> Option<String> {
        let notice = self.fallback_continuation.take()?;
        if self.hub.has_queued_interrupt() {
            return None;
        }
        Some(notice)
    }

    /// **The turn a fallback earned is running** — the one-shot, seen from inside it: read by
    /// [`Harness::fall_back_from`], which moves the session to the next name in the list but does
    /// not arm a second continuation.
    pub(crate) fn in_fallback_continuation(&self) -> bool {
        self.continuation_of_fallback
    }

    /// Marked by `turn_tail::run_turn` around the continuation turn and cleared after it, so the
    /// flag can never outlive the turn it describes.
    pub(crate) fn set_fallback_continuation(&mut self, on: bool) {
        self.continuation_of_fallback = on;
    }

    /// **A prompt through the seam every session has — the one a child was missing.**
    ///
    /// The operator's requirement, in their words: *"subagent is a normal session.
    /// it must be able to compact on tool call overrun"* — and their question, *"we
    /// have overrun protection exactly for this case, why this resurfaces again and
    /// again"*. Measured 2026-10-06, and this method is the answer to both: a child
    /// of `s-1789462738453908838` stopped after 207 rounds at 1,131,717 of 1,146,137
    /// tokens, its wall notice promised *"a compaction was ATTEMPTED"*, and the
    /// store shows ONE transcript for it — no fork, no compaction item — while its
    /// parent compacted 57 times in the same window. The protection existed; the
    /// child could not reach it.
    ///
    /// # Where the seam was, and why a child never passed it
    ///
    /// The wall is DETECTED here in the harness, between one round's tool results
    /// and the next round's send (the round-loop check in [`Self::submit`] that
    /// returns [`HarnessError::ContextWall`]) — so a child at the wall stops exactly
    /// as a root does. What REACTS to the wall — compact, then continue on the
    /// summary — lived only in `Sessions::after_turn` and
    /// `Sessions::compact_if_at_the_wall`, and a child never gets there:
    /// [`HarnessTaskRunner::run_to_completion`] drives `submit` directly on the
    /// thread its parent spawned it on, and the child is adopted into the registry
    /// and NOT into `Sessions::open` (by R58's design — the daemon cannot run its
    /// turns), so even a compaction attempted by id would find nothing to compact.
    /// A dead child with a notice that says a compaction was attempted is the
    /// sentence `HarnessError::ContextWall`'s Display writes about `Sessions`'
    /// behaviour — true for every session the daemon holds, false for every child,
    /// every time.
    ///
    /// # What this adds over [`Harness::submit`]
    ///
    /// Exactly the tail the daemon gives its own sessions, no more:
    ///
    /// * the wall is checked BEFORE the send, as `Sessions::run_prompt` does for
    ///   the first prompt after a resume;
    /// * a turn that comes back [`HarnessError::ContextWall`] is compacted — the
    ///   same `should_compact` predicate the round loop just fired on — and
    ///   continued by [`Harness::continue_after_wall`], gated on
    ///   [`Config::room_for_next_turn`] and bounded by
    ///   [`WALL_CONTINUES`](crate::sessions::WALL_CONTINUES), the one loop every door
    ///   runs;
    /// * every announcement is the same line the daemon's session gets, named with
    ///   this session's id — the log contrast that settled the finding (a parent
    ///   with `compacting:` lines beside a child with none) is evidence this must
    ///   never leave unreadable again.
    ///
    /// **Since R68.4 there is one tail, not two.** This method was a second copy of
    /// the daemon's (`Sessions::after_turn`, which kept its own because it owned the
    /// failure publish, the todo nag and the turn clock), and the copies drifted. Both
    /// are now [`crate::turn_tail::run_turn`]: this is that tail with no owner around
    /// it ([`crate::turn_tail::Bare`]), the daemon's sessions run it with `Sessions` as
    /// the owner, and a subagent's thread runs it with [`ChildTurns`].
    pub fn submit_as_a_normal_session(&mut self, prompt: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken_as_a_normal_session(Speaker::Operator, prompt)
    }

    /// **A parent's message to a child BETWEEN turns — the turn it makes, spoken as the
    /// parent's.**
    ///
    /// The operator's ruling, verbatim: *"fix task_message - it should enqueue"*. Until this
    /// existed, a message could only enter a turn that was already running (`HubSteering`'s
    /// `Message` arm), and the parent's tool refused to send one to a child that was not —
    /// because *"nothing drains a child's queue between turns"*, so a queued message would
    /// have been accepted and heard by nobody. A child between turns is now reached: the
    /// message is queued, the child's own serving thread is woken (`Hub::wake_its_own_reader`),
    /// and this is the turn that reads it.
    ///
    /// **Why this is not [`Harness::submit`]**: the speaker. The words are recorded as the
    /// sender's — [`message_from_a_session`], naming which session — so they cannot authorise the
    /// act they race, cannot coalesce with the operator's own text, and cannot be taken back.
    /// That is the same rule the mid-turn door follows, and the same text: a session cannot tell
    /// which state it was in when the other end spoke.
    pub fn submit_a_parents_message(
        &mut self,
        from: &str,
        text: &str,
    ) -> Result<Reply, HarnessError> {
        let said = message_from_a_session(from, text, self.cfg.parent_session_id.as_deref());
        self.submit_spoken_as_a_normal_session(Speaker::Agent, &said)
    }

    /// **The turn and its tail, with the speaker given** — the body both doors above share, and
    /// since R68.4 nothing but the shared tail itself ([`crate::turn_tail::run_turn`]) with no
    /// owner around it ([`crate::turn_tail::Bare`]): the pre-wall check, the failure, the
    /// compaction after the turn (the refused door after a wall) and the continuation past it.
    /// This used to be a second copy of that sequence, and the copy was a step behind the
    /// daemon's: no failure published, a different `auto_compact_skipped` sentence.
    fn submit_spoken_as_a_normal_session(
        &mut self,
        speaker: Speaker,
        text: &str,
    ) -> Result<Reply, HarnessError> {
        let door = match speaker {
            Speaker::Operator => crate::turn_tail::Door::Prompt,
            _ => crate::turn_tail::Door::Message,
        };
        crate::turn_tail::run_turn(self, &mut crate::turn_tail::Bare, door, |h| {
            h.submit_spoken(speaker, text).map(Some)
        })
        .expect("a spoken turn always runs")
    }

    /// **A parent's or peer's message as a turn, bare** — the words
    /// [`Harness::submit_a_parents_message`] speaks, without the tail around them, for a caller
    /// that runs the tail itself ([`crate::turn_tail::run_turn`]).
    pub fn parents_message_turn(&mut self, from: &str, text: &str) -> Result<Reply, HarnessError> {
        let said = message_from_a_session(from, text, self.cfg.parent_session_id.as_deref());
        self.submit_spoken(Speaker::Agent, &said)
    }

    /// §5.3: change the system prompt mid-session, in the form the dialect can
    /// render.
    ///
    /// **Appended, never rewritten.** Rewriting message 0 cost a full cold
    /// re-prefill of a 179k conversation when measured; that is what this method
    /// exists not to do.
    pub fn system_update(&mut self, text: &str) -> Result<Reply, HarnessError> {
        self.system_updates += 1;
        // A turn, so distances stay right — but **not** an utterance. A system
        // update is the operator changing the instructions, not the operator
        // authorising an action, and letting it into the trail would make
        // "be helpful with files" readable as consent to touch one.
        self.trail.begin_turn();
        let item = self
            .cfg
            .dialect
            .wiring(self.cfg.effort.as_deref())
            .system_update(self.system_updates, text);
        self.submit_item(item)
    }

    /// **Compaction.** One summary turn, then the fork.
    ///
    /// The turn asks for the summary over the prefix the server is already holding
    /// — that is the whole cache argument, and [`run_compaction`] carries it. If
    /// the model answers by proposing tool calls, the fork is refused: a summary is
    /// a record, not an action, and a summary turn that *worked* is a turn that did
    /// something with nobody watching. The instruction and the turn stay in the
    /// transcript either way — nothing is reduced until the fork lands, so a
    /// refusal here leaves the session exactly as it was, and the next attempt
    /// appends a fresh instruction over this one.
    /// **Who answers this session's summary turns.**
    ///
    /// The provider when there is one, so a compaction is rendered, counted and
    /// billed as part of the conversation it is summarising. Compaction used to
    /// call the engine's local path unconditionally, which sent a metered
    /// session's whole history to the daemon's own model — and a 1.5M-token
    /// history to a 262144-token server is a 400, which is why a conversation
    /// that had overrun could not compact its way out.
    ///
    /// Borrowed from three separate fields, so the caller can still take
    /// `&mut self.engine` and `&mut self.session` beside it.
    fn answerer<'p>(
        provider: &'p Option<Box<dyn letibot_backend::MessagesBackend>>,
        prefix: &'p StablePrefix,
    ) -> letibot_turn::compaction::Answerer<'p> {
        match provider {
            None => letibot_turn::compaction::Answerer::Local,
            Some(p) => letibot_turn::compaction::Answerer::Provider {
                backend: p.as_ref(),
                system: &prefix.system,
                // **The prefix's own tools, so the summary is sent under the SAME
                // prefix as every other turn in this conversation.** They are not here
                // so the summary can call one — nothing executes what it proposes.
                // They are here so a local server's cached prefix still matches and
                // the largest call in the session is not a cold prefill. See
                // `Answerer::Provider`'s docs.
                tools_json: &prefix.tools_json,
            },
        }
    }

    fn submit_item(&mut self, item: TranscriptItem) -> Result<Reply, HarnessError> {
        self.submit_items(vec![item])
    }

    /// [`Harness::submit_item`] for a turn that opens with more than one row — the held batch
    /// before the words that started the turn (R79).
    fn submit_items(&mut self, items: Vec<TranscriptItem>) -> Result<Reply, HarnessError> {
        self.submit_items_then(items, |_| {})
    }

    /// [`Harness::submit_items`], with `opened` called once the rows are in the transcript AND
    /// the store — the point past which the model reads them whatever the rounds come to — and
    /// not at all when the turn never got there (R91.11: `Harness::wake` marks results told
    /// there, not before).
    fn submit_items_then(
        &mut self,
        items: Vec<TranscriptItem>,
        opened: impl FnOnce(&mut Self),
    ) -> Result<Reply, HarnessError> {
        // **The prompt's clock starts before the rows go in** — from the moment a turn has words
        // to give the model it is in flight, and the append below publishes, so a head that was
        // waiting on this session reads `turn` and not a quiet moment between the queue and the
        // model. Idempotent: a wall-continuation comes back through here with the clock already
        // running. See [`Harness::begin_turn_clock`] for why the door does not start it.
        self.begin_turn_clock(letibot_sessionlog::event::now_ms());
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session.append_items(&self.engine, &items, &mut sink)?;
        self.reconcile(&mut sink, &items);
        self.persist()?;
        opened(self);
        self.name_from_first_message();
        let out = self.run_rounds();
        self.note_turn_ended();
        out
    }

    /// **A turn's rounds are over** — counted on the watcher set's read record, which is how the
    /// plan check's grace after an ending tells *delivered in the turn that just ended* from
    /// *delivered a turn ago* (`JobWatchers::grace_left`). Called by both doors into
    /// `run_rounds`, whatever the rounds came to: a turn that failed still ended.
    fn note_turn_ended(&self) {
        if let Some(w) = &self.job_watch {
            w.turn_ended();
        }
    }

    /// Give an unnamed session a name, **once**, from the message that opened it.
    ///
    /// `Config::title`'s note said a title must not be derived from a prompt, and the
    /// reason it gave is the one this respects: *"a title guessed from content is a
    /// title that changes under you, and a session picker whose rows rename
    /// themselves is a picker you cannot learn."* The defect there is the **changing**,
    /// not the deriving. This fires on the first user item of a session that has no
    /// title, writes it to the store, and can never fire again — a second user message
    /// finds a title already set and leaves it alone. So a row's name is stable from
    /// the moment it first has one, which is the property that made the objection.
    ///
    /// It is a default, not a decision: `/rename` and `letibot --rename` overwrite it,
    /// and an operator who names a session up front never sees this run at all.
    fn name_from_first_message(&mut self) {
        if !self.cfg.title.is_empty() {
            return;
        }
        // Read, not remembered. A head can name this session through the registry's
        // own connection at any moment, and this harness's `cfg.title` would still be
        // empty — so a derivation that trusted its own copy would overwrite the name
        // the operator had just typed, with a few words from a message they sent
        // before they typed it.
        if let Some(store) = &self.store
            && let Ok(Some(t)) = store.title(&self.cfg.session_id)
        {
            self.cfg.title = t;
            return;
        }
        let first_user = self.session.items.iter().find_map(|i| match i {
            TranscriptItem::User { parts, .. } => parts.iter().find_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            }),
            _ => None,
        });
        let Some(text) = first_user else { return };
        let title = derive_title(text);
        if title.is_empty() {
            return;
        }
        if let Some(store) = &self.store
            && let Err(e) = store.set_title(&self.cfg.session_id, &title)
        {
            {
                // Not fatal and not silent: an unnamed session is usable, and a
                // store that refused a title is a fact about the store.
                self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "title_not_stored".into(),
                    detail: format!("this session could not be named in the store: {e}"),

                    compaction: None,
                });
                return;
            }
        }
        self.cfg.title = title.clone();
        self.hub
            .publish(letibot_sessionlog::SessionEvent::SessionRenamed {
                title: title.clone(),
            });
        self.new_title = Some(title);
    }

    /// A title this harness has just derived, for the daemon to put in the registry.
    ///
    /// The registry is what a head's picker is drawn from and the harness does not
    /// hold one — `Sessions` does. Handing it over rather than reaching for it keeps
    /// the harness free of the registry, which is what lets a `Harness` be built in a
    /// test with nothing but a `Hub`.
    pub fn take_new_title(&mut self) -> Option<String> {
        self.new_title.take()
    }

    /// Rename this session: the store, the log and this harness's own config.
    pub fn rename(&mut self, title: &str) -> Result<(), HarnessError> {
        if let Some(store) = &self.store {
            store
                .set_title(&self.cfg.session_id, title)
                .map_err(|e| HarnessError::Store(e.to_string()))?;
        }
        self.cfg.title = title.to_string();
        self.hub
            .publish(letibot_sessionlog::SessionEvent::SessionRenamed {
                title: title.to_string(),
            });
        self.new_title = Some(title.to_string());
        Ok(())
    }

    /// **Name the worktree and branch this session's work must end up on** — a `task_start`
    /// child's, set by its spawn. `None` clears it. See [`deliverable`].
    pub fn set_deliverable(&mut self, d: Option<deliverable::Deliverable>) {
        self.deliverable = d;
    }

    /// What a resume rebuilt, or `None` for a session opened fresh.
    pub fn resumed(&self) -> Option<&ResumeReport> {
        self.resumed.as_ref()
    }

    /// The notes `open` made about a session it did not resume — see the field.
    /// Empty for a resumed session, whose notes are in [`Harness::resumed`].
    pub fn open_notes(&self) -> &[String] {
        &self.open_notes
    }

    /// **A turn's rounds, and what is left of its steering when they end** (R91.1).
    ///
    /// The rounds hand back to [`Self::handed_back`] whatever a failed or interrupted round
    /// had taken out of the hub; while the turn runs, the next round's greedy poll takes it.
    /// When the turn is over:
    ///
    /// * **an interrupt is spent** — the turn it was pressed against has ended, and one
    ///   left behind would stop the NEXT turn at its first token;
    /// * **another session's message is WRITTEN INTO THE TRANSCRIPT** — see
    ///   [`Self::keep_the_words_it_never_read`], and `HubSteering`'s `Message` arm for why the
    ///   row and the injection are one item on every other path;
    /// * **everything else stays**, for the next turn's greedy poll: the operator's words, an
    ///   answer the board already took, results already marked delivered. A failed turn read
    ///   none of it, and the operator is told so.
    fn run_rounds(&mut self) -> Result<Reply, HarnessError> {
        // **The clock starts here for a door that appends no rows of its own** — the operator's
        // `!` line is already two rows on the log when `run_after_operator_shell` asks for its
        // turn, so this is the first point that turn exists. A no-op when `submit_items_then` has
        // already started it, which is every other door. See [`Harness::begin_turn_clock`] for
        // why a door that finds nothing to say must not have started it at all.
        self.begin_turn_clock(letibot_sessionlog::event::now_ms());
        let out = self.rounds();
        let (unread, left) = {
            let mut held = self.handed_back.lock().unwrap_or_else(|e| e.into_inner());
            held.retain(|m| !m.urgent);
            // **A session's message comes out of the carried set, because it is about to be a
            // row.** It is not re-offered by any source of its own, so leaving it here is the
            // one way a parent's words could be read twice (the next turn's greedy poll would
            // append the item a second time). See `SteeringMessage::from_session`.
            let mut unread: Vec<SteeringMessage> = Vec::new();
            held.retain(|m| match m.from_session.is_some() {
                true => {
                    unread.push(m.clone());
                    false
                }
                false => true,
            });
            (unread, held.len())
        };
        for m in &unread {
            if let Err(e) = self.keep_the_words_it_never_read(&m.text) {
                // Not fatal: the words are still on this session's log (the door recorded
                // them), and the turn's own outcome is what the caller is waiting for. Said,
                // because a row that was not written is a row a later reader will not find —
                // and **put back**, so the next turn's greedy poll carries them as the
                // injection they would have been rather than losing them to a store that was
                // busy at the wrong moment.
                self.handed_back
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(m.clone());
                self.warn(
                    "message_not_stored",
                    format!(
                        "a message from another session was not written into this session's \
                         transcript: {e}. It is on this log, and it rides the next turn instead."
                    ),
                );
            }
        }
        if left > 0 {
            self.hub.publish(SessionEvent::Warning {
                code: "steering_carried".into(),
                detail: format!(
                    "{left} message(s) that reached this turn while it ran were not read by \
                     the model before it ended; they go in at the start of the next turn."
                ),
                compaction: None,
            });
        }
        out
    }

    /// **A message this turn never read, made a row anyway** — the one path on which a parent's
    /// words could reach a child's queue and leave no record of themselves.
    ///
    /// The ordinary path needs nothing: the engine appends the steering item at a step boundary
    /// and `run_rounds` persists it with the round's own rows (see `HubSteering`'s `Message`
    /// arm for the whole of that argument). This is the other path — a round that failed after
    /// the greedy poll had taken the message, which hands it back
    /// ([`letibot_turn::steering::SteeringSource::give_back`]) with no boundary ever arriving.
    /// The carried set survives a turn and would carry it into the next one, and that is enough
    /// for the operator's own prompt (a head is waiting on it, and the echo says `queued`). It
    /// is NOT enough for a session's message, for two reasons measured here:
    ///
    /// * **a child may never run another turn.** A parked child whose parent spoke while a
    ///   round was failing would hold the words in memory for ever — no row, no queue entry, and
    ///   nothing in the store for a later reader (the operator's own report: four
    ///   `task_message`s, *"of the child's 278 transcript rows, not one contains the parent's
    ///   words"*);
    /// * **the row is what makes the exchange visible at all.** A head attached to the child,
    ///   and the child's own conversation view, read the transcript and nothing else — so words
    ///   that exist only in a steering slot are words nobody can see.
    ///
    /// So the words are appended as the very item the injection would have been — same text,
    /// same `Speaker::Agent`, one `Text` part — and the next turn reads them as the history they
    /// now are. The trail is not told again: the door recorded the utterance when it took it
    /// (`HubSteering`'s `Message` arm), and a second `say` would be a second turn's worth of
    /// authority over one sentence.
    fn keep_the_words_it_never_read(&mut self, said: &str) -> Result<(), HarnessError> {
        let item = TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text {
                text: said.to_string(),
            }],
        };
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session
            .append_items(&self.engine, std::slice::from_ref(&item), &mut sink)?;
        self.reconcile(&mut sink, std::slice::from_ref(&item));
        self.persist()?;
        self.hub.publish(SessionEvent::Warning {
            code: "message_carried".into(),
            detail: format!(
                "a message from another session reached this turn while it ran and the turn \
                 ended before the model read it; it is in the transcript now, so the next turn \
                 reads it there: {said}"
            ),
            compaction: None,
        });
        Ok(())
    }

    fn rounds(&mut self) -> Result<Reply, HarnessError> {
        // **The key first, before anything is drained or sent.** A provider with no key
        // is asked for one here; a refusal returns before the turn takes the operator's
        // message, so nothing they typed is spent on a turn that could not run.
        if self.key_wanted.is_some() {
            self.obtain_key(None)?;
        }
        // **And no model at all is said here too, at the same point and for the same reason.**
        // A daemon started with no GGUF and no provider (`Parts::load`) has nothing a turn can
        // go to: its own server would be sent byte ids it cannot read. The head opens its
        // model chooser before this is reached; this is the floor under a head that did not
        // (a one-shot, an older head), and it names the verb that ends it.
        if self.provider.is_none() && self.engine.vocab().is_bytes() {
            return Err(HarnessError::Setup(NO_MODEL_CHOSEN.to_string()));
        }
        // How many times this turn has asked again after a refused key. Bounded, so a
        // provider that refuses every key does not hold the worker in a loop of cards.
        let mut key_asks = 0usize;
        let mut metrics = Vec::new();
        let mut tool_calls = 0usize;
        let mut truncated = false;
        // **The intent check nudges once per user turn, and the bound is the point.**
        //
        // T21.3's response is *"append 'you said you would X — do it or say why
        // not' and continue"*, and continuing means the answer turn is not the end
        // of the loop. What it must not become is a loop: a model that answers the
        // nudge with more prose and no tool call would produce a second finding, a
        // third, and burn `max_tool_rounds` on an argument. One is the error signal;
        // two is the harness insisting. If the model explains itself and stops, that
        // is a legitimate answer to the check — `docs/closed-loop.md`'s tolerance
        // band, set at its narrowest until somebody measures a better one.
        let mut nudges_left = 1usize;
        // **And a worktree child that is about to finish with its work outside its branch** —
        // `deliverable::EndOfWork`, the same budget of one: told what is uncommitted, once, and
        // then its answer stands (the parent's completion carries the line if it is still dirty).
        // The report: *"Seven times tonight a child has ended with its work outside its branch,
        // and each time the recovery was the same two commands."*
        let mut end_of_work = deliverable::EndOfWork::new();
        // **And a model that stops because "my context is full" when it is not** — R71,
        // `crate::context_claim`. The same budget as the nudge above, and for the same
        // reason: one line telling the model its measured fill, then — if it says it again
        // — one warning to the operator, never a second line to the model. Always on: it
        // is gated by a measurement, not by the prose heuristic `--intent-prose` guards.
        let mut context_check = crate::context_claim::Check::new();
        // **The encoder for "is this turn getting anywhere".**
        //
        // Per user turn, because "already seen" is a claim about *this* turn — a
        // file read in an earlier turn is a legitimate thing to read again now.
        // `max_tool_rounds` below is the backstop it demotes; see `crate::progress`
        // for why a round count was the wrong instrument and what replaced it.
        let mut progress = crate::progress::ProgressDetector::new(self.cfg.stall_rounds);
        // **The turn's standing-notes offers** — see `crate::notes_offer`, which is
        // where the whole argument lives. Per ROUND for the round's own offer, and
        // **per SESSION for the memory** — *"rare (once per note per turn at most)"*
        // was the wrong unit: a turn is minutes and a session is days, so a note
        // never read and never in the prompt won the same slot in turn after turn.
        // Measured 2026-10-10: 120 offers, 19 taken, one note offered 22 times, every
        // repeat a row in the operator's transcript. `recall` reads back what this
        // session has already been offered, so the offer is once per note per
        // SESSION, and does so from the log rather than from a set in memory —
        // see its own doc. The offer itself is per ROUND and lives in the request
        // assembly below, never in the transcript — *a row cannot be un-said; an
        // assembly can be recomposed*.
        let mut offers = crate::notes_offer::Offers::new();
        offers.recall(&self.cfg.daemon_logs(), &self.cfg.session_id);

        let backstop = round_backstop(self.cfg.max_tool_rounds);
        for round in 0..backstop {
            // **The wall can arrive MID-TURN, and the turn boundary is too late.**
            //
            // A tool result is appended between rounds, so one big one can put the
            // conversation past `n_ctx` while the turn is still running; the next
            // round then sends a prompt the server refuses with
            // `500 Context size has been exceeded` and the WHOLE TURN is lost with
            // nothing recorded. That is how the operator's session died on
            // 2026-09-15 — a 123k-token `edit` refusal, mid-turn.
            //
            // Measured after the boundary check was added and before this one: a
            // 700-line read at `--context-window 8000` left 11,818 tokens
            // resident, 3,818 PAST the window, and survived only because the model
            // happened not to need another round.
            //
            // pi puts a `prepareNextTurn` hook here for the same reason — its own
            // comment says *"Preparation can be long-running (for example,
            // compaction)"* — and runs it between rounds rather than around the
            // turn.
            //
            // This stops the turn instead of compacting in place, because
            // `turn::compaction` is explicit that building the new base reaches
            // the store, the resume chain and the registry, none of which a
            // `Harness` owns. Stopping is the honest half a `Harness` CAN do: the
            // rounds so far are committed, `Sessions` compacts at the boundary it
            // already checks, and the next turn continues on the summary. A turn
            // that ends early and says so beats one the server refuses whole.
            // `!self.compacting`: **the compaction turn is exempt.** It runs
            // through this same loop, and it is the one turn whose whole purpose
            // is to be over the wall — stopping it there would make compaction
            // impossible exactly when it is needed, which is what happened on the
            // first run of this check.
            // **Measured before it is judged (R72)**: the tool results of the round just
            // ended are in the ledger and not in the last round's count, and the roll adds
            // them as sent rather than through a ratio they have nothing to do with.
            //
            // **And round 0 is checked too, against the window itself** (R72 follow-up).
            // The doors before a turn (`Sessions::run_prompt`, the nag, the wake) judge the
            // conversation BEFORE the prompt is appended, with a reserve of a sixteenth of
            // the window for it — so a prompt bigger than that reserve (a paste, a long
            // message from a session) went out over the window and came back as the
            // provider's refusal, the wall reached by a wasted request. Round 0 stops only
            // when what it is about to send is already PAST the window, not inside the
            // reserve: the reserve is the doors' question, and asking it again here would
            // stop every turn the doors had just let through near the edge. Between rounds
            // it is the reserve, as before — the next round's reply has to fit too.
            let now = (!self.compacting).then(|| self.measure_resident());
            // **Not on an estimate of a folded base, at round 0, on a metered session**:
            // that count is the fork's re-based estimate, and the one round that would
            // replace it with the provider's is this one. Stopping it on the estimate
            // (pg-noop, 2026-10-11: ≈ 1,039,055, past the window; measured 507,355) is a
            // wall nothing could ever measure its way out of. If the provider counts it
            // over, it says so, and the refused door folds.
            let unmeasured_base = round == 0
                && self.provider.is_some()
                && self.cfg.wall.awaiting_on(&self.session.transcript_id);
            if !self.compacting
                && !unmeasured_base
                && let Some(window) = self.cfg.planning_window()
                && self.cfg.auto_compact
                && !self.cfg.wall.is_stuck()
            {
                let resident = self.session.ledger.len() as u64;
                let prefix = u64::from(self.session.ledger.prefix_len());
                if wall_stops_this_round(
                    round,
                    resident,
                    prefix,
                    self.cfg.headroom(),
                    window,
                    self.cfg.wall_basis(),
                ) {
                    // In the units the operator's header shows, and the count the
                    // decision was made on; see `Harness::measure_resident`. The
                    // ledger's own figures are named once at the end rather than per
                    // number, because three conversions in one sentence is not a
                    // sentence.
                    let (w, h) = self.cfg.shown_wall();
                    let r = now.map(|n| n.tokens).unwrap_or(resident);
                    let unit = self.token_unit();
                    let here = self
                        .provider
                        .as_ref()
                        .map(|p| format!("{}/{}", p.name(), p.model()))
                        .unwrap_or_default();
                    let also = if let Some(model) = self.cfg.carried_note() {
                        // **An estimate, named as one, with whose measurement it is.** This
                        // is the line the 2026-10-25 session needed and did not get: the
                        // count is the ledger's bytes converted by the ratio deepseek
                        // measured, not a number anything has confirmed for glm-coding.
                        format!(
                            " (estimated with {model}'s measured scale: no round has run on \
                             {here} yet, so this box's own ledger count is converted by the \
                             ratio {model} measured before the switch. One round here replaces \
                             it)"
                        )
                    } else if self.cfg.tokens_are_converted() {
                        format!(
                            " (counted as the provider counts them; this box's own \
                             ledger says {resident} of {window}, and it over-counts \
                             because it holds the reasoning the provider is not sent)"
                        )
                    } else if self.cfg.tokens_are_unscaled() {
                        // **The switch window, named rather than presented as calibrated.**
                        // No round has run on this model yet, so these are the ledger's own
                        // counts against a window measured on the model before it — and a
                        // reader who is not told cannot tell that from a measured sentence.
                        format!(
                            " (these are this box's own ledger counts: no round has run on \
                             {here} since it took over, so nothing has measured how its \
                             tokens relate to the ledger's. One round sets it)"
                        )
                    } else {
                        String::new()
                    };
                    self.hub.publish(SessionEvent::Warning {
                        code: "context_wall".into(),
                        detail: format!(
                            "stopping this turn after {round} round(s): {r} of {w} \
                             {unit} are resident and the next round needs {h} free{also}. \
                             Everything so far is committed, and the session compacts \
                             before the next turn — this is the wall, not a failure of \
                             the work."
                        ),

                        compaction: None,
                    });
                    // **And it names the session, for the same reason the `compacting:` line
                    // two steps below it does and only since 2026-10-03.** This is one log file
                    // every daemon appends to, so a wall printed here and the compaction it
                    // triggers (`sessions.rs`) land with other sessions' work between them —
                    // and with no id either side, *which* session met the wall is unreadable,
                    // which is exactly what left the operator's *"too much too early"*
                    // unattributable. Measured 2026-10-03: a wall at 1.4M tokens followed by a
                    // fold to 633k and a compaction to 8k, and no way to say whether those were
                    // one session behaving differently or two behaving consistently. The id
                    // costs fourteen bytes and makes the pair a measurement rather than an
                    // adjacency — the same correction the `compacting:` line already had.
                    let session_id = &self.cfg.session_id;
                    eprintln!(
                        "  {session_id}: context wall: stopped after {round} round(s) at {r} of {w} \
                         {unit} ({resident} of {window} ledger tokens)"
                    );
                    return Err(HarnessError::ContextWall {
                        rounds: round,
                        resident: r,
                        window: w,
                        unit,
                        ledger: (resident, window),
                    });
                }
            }
            let mut sink = CapturingSink::new(self.hub.clone());
            // **Compose this round's offer, before anything is sent.** It is scored
            // against the person's prompt and the turn's tool results — the design's
            // own subject — and handed to the engine as a trailing item that
            // `session.items` never sees. Nothing over the floor is said, and
            // nothing said is recorded: see `crate::notes_offer`.
            //
            // **Not on a compaction turn.** That turn is the harness asking for a
            // summary of what is already here; a note offered on top of it is a
            // fetch in the middle of a job whose whole instruction is to write, and
            // the offer would be settled against calls the summary turn never made.
            if !self.compacting {
                offers.compose(
                    &self.cfg.workspace,
                    &crate::standing_notes::global_dir(),
                    &crate::leticode_config::BoxDirs::for_this_box(),
                    &self.prefix.system,
                    &self.session.items,
                );
            }
            let tail = offers.tail();
            // **Take the round again when the endpoint is the thing that
            // failed.** An inner loop, so a retry does NOT spend one of
            // `max_tool_rounds`: it produced nothing and appended nothing, and
            // ending a turn early because a server was restarting would be the
            // budget measuring the wrong thing. See `letibot_http::retry`.
            //
            // # And the second question: is the operator's `/models` being heard?
            //
            // It is, and it is answered HERE — but it is not APPLIED here, and that
            // distinction is the whole of this loop's model rule.
            //
            // MEASURED 2026-10-12, the operator's GLM coding plan, quota exhausted:
            // `/models` sat in the hub's queue for a whole 63-second ladder and was
            // answered after the turn, by which time the turn was FAILED — *"while
            // those backoffs were cycling - the model change didnt take"*. So the turn
            // polls the queue itself and answers (`apply_queued_model`).
            //
            // MEASURED 2026-10-26, the other half: applying it here is what broke the
            // next session. The round was re-issued on deepseek carrying the rest of
            // GLM's turn — assistant tool-call rounds with no `reasoning_content` — and
            // deepseek's thinking mode refuses exactly that (`http 400: The
            // `reasoning_content` in the thinking mode must be passed back to the API`).
            // The operator's sentence for the rule: *"we can switch after turn ends"*.
            //
            // So: the switch is recorded and said (*"turns go to X when this turn
            // ends"*), every attempt of this turn is sent to the model the turn STARTED
            // on, and `end_turn_clock` applies it. `[fallback]` is the same rule for the
            // daemon's own switch — see `fall_back_from`.
            let mut tried_fallbacks: Vec<String> = Vec::new();
            // **One steering per round, outside the retry loop (R91.1).** A failed attempt
            // hands what it took back to this value, and the retried attempt's greedy poll
            // must find it there; one built per pass dropped it on the floor.
            let mut steering = self.steering();
            // **The ladder this turn walks, and the only thing that decides whether a round
            // is taken again** — see `letibot_http::retry`, where *what is retryable, and for
            // how long* is written down. It is also what the turn's ending is said from: the
            // count and the waiting are nobody else's to report, and a failure that reads as
            // `FAILED — http io: …` with no count is the report this was written for.
            let mut ladder = letibot_http::retry::Ladder::new(letibot_http::retry::Policy::new(
                self.cfg.http_retries,
            ));
            let outcome = loop {
                // **The operator's own line, answered before the attempt rather than after
                // it** — the window they type in. What comes back says when the switch
                // takes; nothing about this attempt moves.
                self.apply_queued_model();
                // **Start watching BEFORE the round, so the wait itself is covered.**
                // The host is `Some` only on the metered route — a local prefill's
                // silence is progress, not a hang; see `SilenceWatch`.
                let cloud_host = silence_host(self.provider.as_deref());
                let watch = SilenceWatch::start(
                    self.hub.clone(),
                    cloud_host,
                    std::time::Duration::from_secs(SILENT_ROUND_SECS),
                );
                let attempted = match &self.provider {
                    None => self.engine.run_turn_steered_with_tail(
                        &mut self.session,
                        &mut sink,
                        &mut steering,
                        &tail,
                    ),
                    // A cloud turn: the transcript as messages, the ledger as the
                    // record. Same events, same verdicts, same TurnOk.
                    Some(p) => self.engine.run_turn_messages_with_tail(
                        &mut self.session,
                        &mut sink,
                        &mut steering,
                        p.as_ref(),
                        &self.prefix.system,
                        &self.prefix.tools_json,
                        &tail,
                        None,
                    ),
                };
                // The round has produced something or failed, either way the silence
                // is over and the watchdog has nothing left to say.
                watch.finish();
                let Err(TurnFailure::Http(e)) = attempted else {
                    // **The new model answered**, which is the whole of what a switch had
                    // to prove — there is nothing to go back to any more.
                    self.switch_back = None;
                    break attempted;
                };
                // **A switch the operator made during this attempt is answered here, and
                // nothing else changes.** The wait below exists for an endpoint that is
                // coming back — a 5xx, a timeout — and a deliberate switch is not weather
                // either way: it is not this round's to apply, so the ladder goes on against
                // the model the turn started on and the switch takes when the turn ends.
                // Answered BEFORE the key arm deliberately: a 401 from the model they are
                // leaving is not a reason to ask them for a key to it.
                self.apply_queued_model();
                // **A refused key is asked for again**, on the same card, and the round is
                // taken again — nothing was recorded, so the retry sends the same bytes.
                // Only a key this session can replace: one from `$PROVIDER_API_KEY` or
                // `--api-key` wins over the file, so a typed one would be saved and ignored.
                if let letibot_turn::HttpError::Status {
                    code: code @ (401 | 403),
                    body,
                } = &e
                    && self.provider.is_some()
                    && key_asks < 2
                    && let Some(pc) = self.cfg.provider.clone()
                    && key_is_replaceable(&pc)
                {
                    key_asks += 1;
                    let why = format!("{} refused the key ({code}): {body}", pc.name);
                    self.provider = None;
                    self.key_wanted = Some(pc);
                    if let Err(e) = self.obtain_key(Some(why)) {
                        // **The turn ends here, so the outage does too.** Left in the field it
                        // would be continued by the NEXT turn's first failure, carrying this
                        // one's start time into a duration that never happened.
                        self.settle_outage(false);
                        return Err(e);
                    }
                    continue;
                }
                // **This attempt failed, and the outage it belongs to remembers how many
                // have** — the record is changed here and published nowhere: the row is
                // written once, when the outage ends. See [`Outage`]. After the key arm
                // deliberately: a key the provider refused is that card's news, not the
                // endpoint's weather.
                self.outage_failed(&e);
                let Some(wait) = ladder.failed(&e) else {
                    // **The ladder has nothing left to wait for, and the session may still
                    // have somewhere to go — but not this round.**
                    //
                    // The MODEL itself is what refused (a 429 or a 5xx, not a wire failure)
                    // and this box has a `[fallback] models` list, so `fall_back_from` names
                    // the first model that can answer and records it as the switch waiting
                    // for this turn to end. **The round is not re-issued there**, and that is
                    // the same rule as the operator's own `/models` above: a model's first
                    // request has to follow a user message, and the rest of this turn belongs
                    // to the model that refused it. So this turn ends here with the provider's
                    // own refusal, and the next one starts on the model the list names.
                    //
                    // A transport failure gets not even that: the model may be perfectly
                    // well and it is the route to it that is down, so moving the session
                    // would abandon the box the operator chose for a reason that is not
                    // about the model.
                    if let Some(next) = self.fall_back_from(&e, &mut tried_fallbacks) {
                        let was = self.provider_line();
                        // **Whether a turn was armed decides what may be promised.** This turn is
                        // the continuation a fallback earns whenever one is already running (the
                        // bound), and then no second turn is opened — so the sentence must not
                        // say one is.
                        let opens_a_turn = self.fallback_continuation.is_some();
                        // **The outage is this notice's to tell.** One row per outage, and
                        // this is the row: the session is moving, and the count belongs in
                        // the sentence that says so rather than in a second row beside it
                        // saying the same endpoint is out. Taken, so `settle_outage` below
                        // finds nothing left to write. See [`Outage`].
                        let outage = self.outage.take();
                        self.hub.publish(SessionEvent::Warning {
                            code: "model_endpoint_retry".into(),
                            detail: fallback_notice(&was, &next, &e, opens_a_turn, outage.as_ref()),

                            compaction: None,
                        });
                    }
                    break Err(TurnFailure::Http(e));
                };
                // **Nothing is published here, and that is the change.** This used to raise
                // `model_endpoint_retry` once per attempt — three rows in three seconds for
                // one endpoint, which is the operator's *"i dont need walls of this … a
                // coutner is what is need"*. The attempt is already in the outage's count
                // (above) and the row is written when the outage ends; see [`Outage`].
                if !self.sleep_unless_closed(wait) {
                    break Err(TurnFailure::Http(e));
                }
            };
            // **The outage this round was in, if any, is over — and it is said ONCE, here,
            // where the count is final.** A round that ended on anything but an HTTP
            // failure was answered: the endpoint is back, and that is the other half of the
            // news (a silent recovery is how somebody spends an afternoon believing it is
            // still down). See [`Outage`].
            self.settle_outage(!matches!(&outcome, Err(TurnFailure::Http(_))));
            // Before the match, deliberately: three of the four arms below leave
            // this function, and the one that matters most for §4.5 is the `Err(e)`
            // that propagates — a failure whose turn id is only recorded on the
            // success path is a failure the daemon cannot name.
            self.last_turn_id = sink.turn_id.clone().unwrap_or_default();
            let ok: TurnOk = match outcome {
                Ok(ok) => ok,
                // §5.7, and the notice goes where the model will read it.
                // Nothing was committed, so there is no tool-call row to answer
                // and the notice is a user item. Bounded by the engine's own
                // salvage budget: the round after this one either succeeds or
                // comes back as `SalvageExhausted`.
                Err(TurnFailure::BatchTruncated { notices, .. }) => {
                    self.append_notice(letibot_plugin::notice::SALVAGE, &notices.join("\n"))?;
                    continue;
                }
                // **The operator's own sentence, because it is the only one with evidence.**
                //
                // Measured in the transcript, 2026-09-18. The model overthought its way past the
                // output limit and said nothing. It was sent three machine notices in a row --
                // "continue or say why not", "Answer again, and put the answer before the
                // reasoning if you are close to the limit", then the first again -- and produced
                // three more empty turns. Then the operator typed:
                //
                //     you keep overthinking cut it short and do things
                //
                // and the very next reasoning block opened "The operator is frustrated. Let me
                // cut it short and just do the thing", followed by the work, in 34 tokens of
                // thinking instead of thousands.
                //
                // Why the old ones failed, specifically:
                //   * "continue or say why not" is an OPEN QUESTION. A model that just
                //     overthought is being invited to deliberate, and it accepts.
                //   * "if you are close to the limit" is a CONDITION it has to evaluate --
                //     more thinking -- and it is already true, so the hedge is pure cost.
                //   * Neither says the thing that worked: stop thinking, act.
                //
                // So: imperative, short, no question, no condition, and it names the behaviour
                // rather than the mechanism. A model does not need to be told about token
                // limits; it needs to be told what to do next.
                // R93.D1: it named the enum (`reasoning_only`), told models whose thinking
                // always comes first to "reason after", and accused on the first occurrence.
                Err(TurnFailure::EmptyLength { .. }) => {
                    self.append_notice(
                        letibot_plugin::notice::NUDGE,
                        "Your reply was all reasoning and nothing was kept. Make the tool call \
                         or give the answer now.",
                    )?;
                    continue;
                }
                // R7: the turn stopped inside its own reasoning block and said
                // nothing. GLM's end-of-turn token doubles as a nameable string,
                // so this is self-inflicted turn-ending, and the answer is the
                // same shape as the two arms above — tell the model, let the loop
                // run. Bounded by the engine's salvage budget exactly as they are:
                // once the cap is spent the engine returns `SalvageExhausted` and
                // this arm never sees another unfinished turn.
                Err(TurnFailure::UnfinishedReasoning { .. }) => {
                    // R93.D2: this arm is the turn ending INSIDE its reasoning, not running out.
                    self.append_notice(
                        letibot_plugin::notice::NUDGE,
                        "Your reply ended inside reasoning with no output. Make the tool call \
                         or give the answer now.",
                    )?;
                    continue;
                }
                Err(TurnFailure::Http(e)) => {
                    // **A context-length refusal IS the wall, and this is where it
                    // becomes one.** The provider owns the window and it has just
                    // said the prompt does not fit; every recovery `Sessions` has
                    // — the forced compaction and the continuation after it — is
                    // gated on `HarnessError::ContextWall`, so leaving the refusal
                    // a backend error leaves all of it unreachable. MEASURED
                    // 2026-10-09, session `s-1789919514688401228`: thirty
                    // consecutive 400s from `deepseek/deepseek-flash`
                    // (1,048,624 / 1,049,070 / 1,049,191 tokens against its
                    // 1,048,576), the first on a `monitor` wake and the last on a
                    // `todo check`, and not one compaction between them.
                    //
                    // The numbers stay honest about whose they are: the ERROR
                    // carries this box's own ledger and planning window (what it
                    // knows, in the units it decides in), and the WARNING names the
                    // provider's two alongside them — never a blend. A window this
                    // box never learned reads as zero in the error and is named as
                    // unconfigured in the warning, because inventing one here would
                    // point the operator at a wall nobody measured.
                    if let letibot_turn::HttpError::Status { body, .. } = &e
                        && let Some(refusal) = context_length_refusal(body)
                    {
                        let r = self.measure_resident().tokens;
                        let resident = self.session.ledger.len() as u64;
                        let window = self
                            .cfg
                            .planning_window()
                            .or(self.cfg.context_window)
                            .unwrap_or(0);
                        let (w, h) = self.cfg.shown_wall();
                        let unit = self.token_unit();
                        let theirs = match (refusal.requested, refusal.limit) {
                            (Some(req), Some(lim)) => format!(
                                " The provider's own count: {req} tokens against a {lim} \
                                 window."
                            ),
                            (Some(req), None) => {
                                format!(
                                    " The provider's own count: {req} tokens; it did \
                                     not name its window."
                                )
                            }
                            (None, Some(lim)) => format!(
                                " The provider named a {lim}-token window but not this \
                                 prompt's size."
                            ),
                            (None, None) => String::new(),
                        };
                        let no_window = if self.cfg.context_window.is_none() {
                            " This box has no --context-window set, so its own checks \
                                 could not have seen this coming; the provider's refusal \
                                 is the first measurement of this session's wall."
                        } else {
                            ""
                        };
                        self.hub.publish(SessionEvent::Warning {
                            code: "context_wall".into(),
                            detail: format!(
                                "stopping this turn after {round} round(s): the PROVIDER \
                                 refused the prompt for length. This box counts {r} \
                                 of {w} {unit} resident with {h} reserved for the next \
                                 round.{theirs}{no_window} Everything already produced is \
                                 committed, and the session compacts before the next turn \
                                 — this is the wall, not a failure of the work.",
                            ),
                            compaction: None,
                        });
                        eprintln!(
                            "  {}: context wall: the provider refused the prompt for \
                             length after {round} round(s) at {r} of {w} {unit} ({resident} \
                             of {window} ledger tokens)",
                            self.cfg.session_id
                        );
                        return Err(HarnessError::ContextWall {
                            rounds: round,
                            resident: r,
                            window: w,
                            unit,
                            ledger: (resident, window),
                        });
                    }
                    // **The whole account of the failure, not just the last call of it.**
                    //
                    // `http io: provider unreachable: io: failed to lookup address information:
                    // Temporary failure in name resolution` is the truth about ONE call, and the
                    // operator read it under `FAILED — ` with `(nothing was recorded)` after it and
                    // no way to tell whether the daemon had tried once or six times — nor, on the
                    // other class, whether a `401` had been asked six times for no reason. The
                    // ladder knows both, so the ending is its sentence: *tried 6 times over 31s,
                    // then gave up: name resolution failed*, or *http 401: … — the provider refused
                    // it, and a refusal is not retried (1 call, no waiting)*.
                    //
                    // The typed cause rides along ([`HarnessError::Endpoint`]) because
                    // `switch_back_notice` still asks it which 4xx refused.
                    return Err(HarnessError::Endpoint {
                        said: ladder.ending(&e),
                        cause: e,
                    });
                }
                Err(e) => return Err(e.into()),
            };

            // **Pair against everything the ledger took, not just the model's rows.**
            //
            // A steering message is appended inside the turn (`engine.rs`: "let
            // steering_items = pending.take_items(); session.append_items(...)"), through
            // the SAME sink, so it emits a `TranscriptAppended` like any other row — but
            // `TurnOk` reports it in `steering_applied` rather than in `items`.
            // Reconciling against `items` alone therefore left one unmatched id per
            // steering message, `record_item_pairing` fired, and the head drew the
            // steering rows empty.
            //
            // Harmless until T21.3 joined the intent check to the steering source, which
            // made an injection ordinary rather than rare. `reconcile` zips positionally,
            // so the order here is load-bearing — and there are now THREE groups, not
            // two, because the greedy poll appends whatever was waiting BEFORE the
            // generation as well. Transcript order is: what arrived while the last tool
            // ran, then the model's own rows, then what arrived while it was speaking.
            let mut appended = ok.steering_before.clone();
            appended.extend(ok.items.iter().cloned());
            appended.extend(ok.steering_applied.iter().cloned());
            self.reconcile(&mut sink, &appended);
            self.persist()?;
            // The last round's prompt size is the session's context size, and the
            // session row is the only place it survives a restart: a head that
            // attaches to a rebuilt view has no turn state to read it from.
            // Per round, the same way the heads' own `TurnFinished` updates theirs,
            // so the row and the screen agree at every point a round has landed.
            // **And what that prompt cost in each side's tokens.**
            //
            // The ledger and the provider count different things — reasoning is in
            // one and dropped from the other — so this is the only honest way to
            // convert between them, and it is a measurement rather than a
            // constant. See `Config::ledger_scale`.
            // **Any round, local included.** The guard here used to be
            // `self.provider.is_some()`, which meant a local round never refreshed the ratio:
            // after a provider → local switch the provider's scale stayed for the rest of the
            // session, and a session that had never been switched ran unscaled for ever. The
            // ledger and the server count the same prompt, so a local round measures a ratio
            // that is 1 by construction — and measuring it rather than assuming it is the
            // same rule the rest of this field follows.
            //
            // **And paired with the ledger the PROMPT had, not the one after the reply
            // (R72).** `prompt_tokens` counts what was sent; the round's own rows — the
            // model's reply and what was steered in while it spoke — were appended after
            // it, so the ledger read here is longer than the prompt by exactly those rows.
            // Pairing the two made a ratio of two moments, and on a reasoning-heavy reply
            // the difference is mostly reasoning. The prompt's ledger is the row boundary
            // before them: the rows are one-to-one with `appended`, and the last
            // `items + steering_applied` of them came after the prompt.
            let rendered_at = {
                let rows = self.session.ledger.rows();
                let after = ok.items.len() + ok.steering_applied.len();
                match rows.len().checked_sub(after) {
                    Some(0) => u64::from(self.session.ledger.prefix_len()),
                    Some(n) => u64::from(rows[n - 1].end()),
                    None => self.session.ledger.len() as u64,
                }
            };
            if ok.metrics.prompt_tokens > 0 {
                self.wall_anchor = Some(WallAnchor {
                    transcript: self.session.transcript_id.clone(),
                    ledger: rendered_at,
                    provider: ok.metrics.prompt_tokens,
                    measured: true,
                    scale: sent_scale(
                        &self.session.ledger,
                        &self.session.items,
                        rendered_at,
                        ok.metrics.prompt_tokens,
                        self.provider.is_some(),
                    ),
                });
                self.cfg.ledger_scale = Some((rendered_at, ok.metrics.prompt_tokens));
                self.measure_resident();
                // **The measurement the wall controller waits for** — the first round on a
                // folded base is where "did the fold make room" is answered, by the
                // provider's own count. Not the summary turn's rounds: those count the
                // history being folded, not the base.
                if !self.compacting {
                    self.observe_wall(ok.metrics.prompt_tokens);
                }
            }
            // Persisted with the ledger beside the count, so a restart recovers the
            // pair above instead of pairing the count with whatever ledger it has
            // then. See the gate in `Harness::open`.
            //
            // **Not fatal (R91.6).** The row is a measurement, and the round it measures is
            // already in the ledger and the store: a busy store that refused it used to end
            // the whole turn. Said, and the next round writes a fresh one.
            if let Err(e) = self.persist_context(&ok.metrics, rendered_at) {
                eprintln!("  store: {e}");
                self.hub.publish(SessionEvent::Warning {
                    code: "context_not_stored".into(),
                    detail: format!(
                        "this round's context size was not written to the session row: {e}. \
                         The turn goes on; the next round's measurement replaces it."
                    ),
                    compaction: None,
                });
            }
            truncated |= ok.truncated;
            // A steering message injected at the step boundary is already in the
            // transcript, and its own `TranscriptAppended` came through the same
            // sink, so `reconcile` picked it up above.
            let calls: Vec<ToolCall> = appended
                .iter()
                .filter_map(|i| match i {
                    TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            let text = visible_text(&appended);
            // **The offer is settled here, where the round's calls are known**, and
            // logged whichever way it went — *"the signal is did a tool call in the
            // next round touch that path? Log offers and take-ups; tune the floor
            // from that ratio."* A round that ends the turn with no calls settles
            // with an empty list, which is an untaken offer and is recorded as one.
            offers.settle(&self.cfg.daemon_logs(), &self.cfg.session_id, &calls);
            // **What the agent says it is doing, recorded before its calls are
            // adjudicated.** A model's prose for a round is the sentence in front of
            // its tool calls — "the queued-prompt rendering lives in app.rs; I am
            // reading the composer block" — and that sentence is the one thing the
            // guard is missing when the operator's instruction names a goal rather
            // than a file. Never an authorisation and never citable; see
            // `ModelBrief::agent_claim`.
            self.trail.claims(&text);
            self.meter_round(&ok.metrics, rendered_at);
            metrics.push(ok.metrics);

            // **The context claim, checked against what the daemon measured** — R71. Judged
            // here, once per round and whichever way the round went, so the log has every
            // claim; acted on in the two branches below, where the intent check steers.
            // **And the reasoning in front of it**, when the reply holds no claim: a model that
            // budgets by "my context 49% → one call" says it there and nowhere the operator reads.
            // A claim found only there is told, never made to keep going — the reply may be whole.
            let (context_claim, claim_in_reasoning) =
                match self.judge_context_claim(&mut context_check, &text, false) {
                    Some(line) => (Some(line), false),
                    None => {
                        let thought = reasoning_text(&ok.items);
                        match self.judge_context_claim(&mut context_check, &thought, true) {
                            Some(line) => (Some(line), true),
                            None => (None, false),
                        }
                    }
                };

            if calls.is_empty() {
                // **A model that stopped "because the context is full" when it is not.**
                //
                // First, ahead of the intent check: the excuse is usually WHY a promise was
                // not kept, and the measured fill is the more useful of the two lines. It goes
                // through `append_notice` for the reason the nudge below does — the turn is
                // over, and a steering message would arrive after the reply — and the loop
                // continues, which is the half that makes it a correction rather than a report.
                if let Some(steer) = &context_claim {
                    self.append_notice(letibot_plugin::notice::NUDGE, steer)?;
                    if !claim_in_reasoning {
                        continue;
                    }
                }
                // **The turn boundary, and the one the error signal is about.**
                //
                // A turn that ends with no tool call is exactly T21.3's case —
                // *"when model says ill start that and by end of the turn forgets
                // and does not start anything"* — so the diff runs here, before the
                // reply leaves. `close_the_turn` reads the assistant's own text out
                // of `appended`; the tool calls are already in the ledger, put there
                // by the encoder as they ran.
                if nudges_left > 0
                    && let Some(steer) = self.close_the_turn(&ok.turn_id, &appended)
                {
                    nudges_left -= 1;
                    // Through `append_notice` rather than the steering queue: this
                    // turn is over, and a message queued for "the next step
                    // boundary" of a turn that has ended would arrive after the
                    // reply the operator is waiting on. The loop continues, which is
                    // the half of T21.3 that makes it a correction rather than a
                    // report.
                    self.append_notice(letibot_plugin::notice::NUDGE, &steer)?;
                    continue;
                }
                // **The branch is the deliverable.** After the intent check, so a turn that
                // promised work and did not start it hears about the promise first; the dirty
                // tree is still there to be told about on the round that answers it. Through
                // `append_notice` for the same reason as the nudge above: the turn is over, and
                // the loop continuing is what makes it a correction rather than a report.
                // **Not on a round that was cut short**: an interrupt (a `job_kill`, the
                // operator's stop) is the turn being ENDED, and a nudge that continued it would
                // be the harness overruling the stop. That child's line goes to its parent in
                // the completion instead (`AnswerOnce`).
                if !ok.truncated
                    && let Some(steer) = end_of_work.at_turn_end(self.deliverable.as_ref())
                {
                    self.append_notice(letibot_plugin::notice::NUDGE, &steer)?;
                    continue;
                }
                // **A message queued while the head was running is a prompt, not a
                // footnote.**
                //
                // The engine drains the steering queue at the step boundary — after
                // `TurnFinished` — and appends what it finds as user items, reporting
                // them in `steering_applied`. Until now that was where the story
                // ended: a turn that made no tool call returned here, the worker went
                // idle, and the queued message sat in the transcript answered by
                // nobody — "message was queued when you stopped and it didnt restart
                // you, while it went out o the queue". The loop continues instead:
                // the next round's prompt is the transcript, the queued items are its
                // last rows, and the model answers them. This covers every kind of
                // steering the boundary absorbs, not only the operator's words — a
                // monitor firing that lands at the boundary is `wake`'s business when
                // nothing is running, and this is the same obligation when something
                // just did. Bounded the way every round is: a round with nothing
                // queued and no tool call returns below, so one queued message costs
                // exactly one round.
                if !ok.steering_applied.is_empty() {
                    continue;
                }
                self.publish_jobs();
                return Ok(Reply {
                    text,
                    metrics,
                    rounds: round + 1,
                    tool_calls,
                    truncated,
                });
            }

            tool_calls += calls.len();
            let turn_id = ok.turn_id.clone();
            let mut backgrounded = false;
            let mut results = Vec::with_capacity(calls.len());
            // **The person's half of a question, held back until every result is in.**
            //
            // A question's answer is the operator's own row, and it is appended with this
            // round's results — but AFTER all of them, never between a call and its result.
            // `Harness::settle_asked` carries the measurement and the reason; the short of
            // it is that a non-`tool` row closes `pair_tool_calls`' answering window, so a
            // `User` row inserted before the result makes the model read *no result was
            // recorded* beside the real one.
            let mut asked: Vec<TranscriptItem> = Vec::new();
            for call in &calls {
                // Call order, and appended in call order. See point 3 above.
                // `self.tool_sink` is the log sink wrapped in the intent encoder, so
                // every result is measured on its way past — an `ok` is an effect
                // and anything else is an attempt, which is the distinction the
                // diff below is built on.
                // **And nothing an earlier call left can be read as this one's answer.**
                // The slot is emptied immediately before the call runs, so whatever is in
                // it afterwards belongs to THIS call — including nothing. Without this a
                // questioner that ran outside the round loop (the door's own execute path)
                // would leave an exchange for the next model question to claim.
                self.asked.lock().unwrap_or_else(|e| e.into_inner()).take();
                let started = Instant::now();
                let r = self.runtime.invoke(&turn_id, call, &mut self.tool_sink);
                self.meter.tool(&self.cfg.session_id, &r, started);
                let mut item = ToolRuntime::transcript_item(&r);
                // **A question the model asked is an exchange with a person, and the
                // person's half of it is theirs.** The tool returns the answer; the
                // session writes it down, here, where every other result is appended.
                if call.name == ASK_USER_QUESTION {
                    asked.extend(self.settle_asked(&mut item));
                }
                // **A plugin layer sees the result before the model does** (`docs/plugins-plan.md`
                // §4.2), and before the harness's own intercept below: the layer is about what the
                // CALL produced, and `pr_watch`'s sentence is the harness's own reading of it.
                if let TranscriptItem::ToolResult {
                    outcome, payload, ..
                } = &mut item
                {
                    // **A successful result only.** A call that failed did nothing to react to,
                    // and its result is the failure's own words — which the model has to read
                    // exactly, not through a layer's opinion of them.
                    let how = match outcome {
                        letibot_transcript::ToolOutcome::Ok => {
                            Some(letibot_plugin::tool_result::OK)
                        }
                        letibot_transcript::ToolOutcome::Backgrounded { .. } => {
                            Some(letibot_plugin::tool_result::BACKGROUNDED)
                        }
                        _ => None,
                    };
                    if let Some(how) = how {
                        self.plugin_result(call, how, payload);
                    }
                }
                // **A pull request this call opened is watched from here on** (`pr_watch`), and
                // the call's own result says so.
                if let TranscriptItem::ToolResult {
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload,
                    ..
                } = &mut item
                    && let Some((url, n)) = crate::pr_watch::opened(call, payload)
                {
                    let said = self.watch_opened_pr(&url, n);
                    payload.push_str(&said);
                }
                // The progress encoder reads the *rendered* result, which is the
                // string the model will actually get to read. Digesting anything
                // else would measure novelty the model never saw.
                if let TranscriptItem::ToolResult {
                    outcome, payload, ..
                } = &item
                {
                    progress.observe(call, outcome, payload);
                    // Backgrounding is the operator reaching for the floor; see the
                    // yield at the end of this round.
                    if matches!(
                        outcome,
                        letibot_transcript::ToolOutcome::Backgrounded { .. }
                    ) {
                        backgrounded = true;
                    }
                }
                results.push(item);
            }
            results.extend(asked);
            let round_verdict = progress.end_round();
            let mut sink = CapturingSink::new(self.hub.clone());
            self.session
                .append_items(&self.engine, &results, &mut sink)?;
            self.reconcile(&mut sink, &results);
            // **And the calls this round's gate admitted without asking anybody** (D16). Said
            // here, after the results and never between them: a notice appended between a call
            // and its result closes the pairing window and the model reads *no result was
            // recorded* beside the real one. See [`Harness::flush_gate_admissions`].
            self.flush_gate_admissions()?;
            // A round can have started or ended a job; refill the mailbox now so a
            // pane opened mid-turn answers with this round's table, not last
            // round's. Cheap: the list is filtered before it is built.
            self.publish_jobs();
            // **And a mode change the operator made while this turn was running.**
            //
            // It sat in the command queue until the worker came back — which is
            // after the turn — so `/mode` moved nothing while the turn went on
            // asking under the old point. That is exactly when somebody reaches
            // for it. Applied here, at the round boundary, on the thread that owns
            // the harness: the gate reads its mode at decision time, so the very
            // next call is governed by it.
            self.apply_queued_mode();
            // **And a tool the operator ran while this turn was running** — R31's deposit,
            // which is what the door is FOR.
            //
            // The operator watches the model go down a wrong path and drops the doc in; the
            // row lands here, at the round boundary, and the next round of this same turn
            // reads it. The alternative was measured rather than feared: a door call sat in
            // the worker's queue until the turn ended, and a turn can run for minutes — see
            // `Hub::try_head_run_command`, which is where the mechanism is written down.
            self.apply_queued_head_run();
            // **And the merge-queue reviews this session hosts**, at the same boundary and for
            // the same reason as the two above: they are work whose whole point is to happen
            // *during* a turn, and the door that would otherwise take them (`Harness::wake`) is
            // the one this turn is holding shut. MEASURED, 2026-10-10: eighteen `Due` rows,
            // none of them ever asked, for half an hour — the ring was queued behind turns that
            // never looked. The cost here is one indexed `SELECT` over a table that is one row
            // per queue entry, against a round that is a generation.
            let served = self.serve_reviews();
            if served > 0 {
                eprintln!(
                    "  {} · reviews -> {served} gatekeeper(s)",
                    self.hub.session_id()
                );
            }
            self.persist()?;
            // The calls of this round have run; if the model revised its plan,
            // the store and the heads hear about it now, at the round boundary —
            // not when the turn ends, which is after the work the plan describes.
            self.flush_todos()?;
            // A turn that ran tools also gets the diff — a completion marked done
            // while nothing succeeded is `CompletedWithoutEffect` and is just as
            // much the error signal. This one goes through the steering queue,
            // because the loop is about to generate again and the step boundary is
            // where §5.8 injects.
            if nudges_left > 0
                && let Some(steer) = self.close_the_turn(&turn_id, &appended)
            {
                nudges_left -= 1;
                self.injected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(steer);
            }
            // And a context claim made by a round that went on to call tools: the model is
            // still working, so the correction rides the step boundary like the nudge above.
            if let Some(steer) = context_claim {
                self.injected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(steer);
            }

            // **A backgrounded job gives the floor back to the OPERATOR, not to
            // the model.**
            //
            // Ctrl-B exists because a tool is taking too long and there is
            // something to say. It used to end only the `bash` call: control went
            // straight back to the model, whose very next move was `job_wait` with
            // a three-minute bound — so the operator was deaf again immediately and
            // their line sat queued through all of it. *"i backgrounded a job but
            // my messsage still queued"*.
            //
            // So when a round backgrounded something and the operator is waiting,
            // the turn ends here. Their prompt is deliberately NOT consumed as
            // steering — `has_queued_prompt` only looks — so the worker picks it up
            // as a turn of its own and answers it first. That is what asking for
            // the floor means; the job is still running and still theirs to read.
            if backgrounded && self.hub.has_queued_prompt() {
                self.publish_jobs();
                return Ok(Reply {
                    text,
                    metrics,
                    rounds: round + 1,
                    tool_calls,
                    truncated,
                });
            }

            // **The stop, and it is two-stage on purpose.**
            //
            // `docs/closed-loop.md` §4: a closed loop corrects inside the tolerance
            // band and faults outside it. One round before the bound the model is
            // told what the detector sees — through the same step-boundary injection
            // T21.3 uses — so a turn that was in fact working can say so and carry
            // on. Only if the *next* round is also stalled does the turn stop.
            //
            // The asymmetry is deliberate. Cutting a working turn has now cost two
            // sessions; letting a stuck one run one extra round costs one round.
            if round_verdict == crate::progress::Round::Stalled {
                if progress.exhausted() {
                    return Err(HarnessError::NoProgress {
                        evidence: progress.evidence(round + 1, self.cfg.max_tool_rounds),
                    });
                }
                if let Some(steer) = progress.nudge() {
                    self.injected
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_back(steer);
                }
            }
        }
        // Unreachable while unbounded — `usize::MAX` rounds is not a number this
        // process reaches — and honest if the bound is ever set back.
        Err(HarnessError::LoopBound { rounds: backstop })
    }

    /// This session's steering source: the head's queue, the intent findings, and
    /// a monitor that fires mid-turn.
    ///
    /// Rebuilt per round because it borrows nothing and holds only clones; what it
    /// must **share** is the trail, the injected queue and the monitor cursor, so a
    /// firing picked up in one round is not delivered again in the next.
    /// **A `/mode` issued while this turn is running**, taken at a round boundary.
    ///
    /// Reported exactly as the between-turns path reports it — same sentence,
    /// same codes — because "what did my mode change do" must not depend on
    /// whether a turn happened to be running when it was typed.
    /// **A door call the operator made while this turn is running**, run and appended here.
    ///
    /// Called at the round boundary beside [`Harness::apply_queued_mode`] and for the same
    /// reason: it is a command whose whole point is to act *during* a turn, so it is taken by
    /// the thread that owns this harness rather than waiting for the worker — which is
    /// inside the turn.
    ///
    /// # The admission is the same one, and it is not a second door
    ///
    /// The name was checked against [`letibot_sessionlog::HEAD_RUN_TOOLS`] on the connection's
    /// thread before this command was ever queued, and the corpus row is written by
    /// [`Harness::admit_operator_call`] — the same function the worker's path calls. So the
    /// sugar does not skip the door: **the same list, the same row, the same `human:<who>`**,
    /// and only the thread that runs the tool differs.
    ///
    /// # It loops, because the operator may drop two things in
    ///
    /// A round is long and the boundary is the only moment this can be done, so a queue that
    /// held two documents would otherwise deliver one per round.
    fn apply_queued_head_run(&mut self) {
        while let Some(cmd) = self.hub.try_head_run_command() {
            // **The operator's shell line is taken by the same picker and runs the
            // same way** — synchronously, here, at the round boundary — but it is
            // not a door call and has no admission to note, so the two kinds are
            // separated before the door's own bookkeeping starts rather than
            // threaded through it with an `if`.
            //
            // **Unless a run of its own kind is already in flight.** One operator run
            // per session is the rule (one input handle; see `crate::bangrun`), and a
            // line typed mid-turn while a between-turns run waits on its thread would
            // overlap it if this ran it here. It parks instead — worker-owned state,
            // drained by the settle arm when the in-flight run hands back — and the
            // turn goes on, which is what the operator watching both wanted: neither
            // their command nor the turn is lost, and neither runs on top of the
            // other. When NOTHING is in flight this keeps its old timing exactly.
            if let CommandKind::OperatorShell { line, who } = &cmd.kind {
                let (line, who) = (line.clone(), who.clone());
                if self.bang_in_flight() {
                    self.park_bang_line(line, who);
                    continue;
                }
                if let Err(e) = self.run_operator_shell(&line, &who) {
                    // Said, not swallowed: the run had no caller to answer, so the
                    // log is the only place the failure can land (the door's arm
                    // below does the same for its own runs).
                    self.import_note(
                        "operator_shell_failed",
                        format!(
                            "`{who}` ran `{line}` from their own console and it could not be \
                             recorded: {e}. The command may have run; what is missing is the \
                             row the next turn would have read."
                        ),
                    );
                }
                continue;
            }
            let CommandKind::OperatorCall {
                call_id,
                name,
                arguments,
                who,
                ..
            } = &cmd.kind
            else {
                // The picker matched an `OperatorCall` or an `OperatorShell`; anything
                // else here would be a bug in the picker, and returning rather than
                // panicking leaves the command queued for the worker, which is where it
                // belongs.
                return;
            };
            let (call_id, name, arguments, who) = (
                call_id.clone(),
                name.clone(),
                arguments.clone(),
                who.clone(),
            );
            // **Noted and taken around a synchronous run**, exactly as the worker's path does.
            // With the daemon running the tool there is no window in which the pending entry
            // is the only record of the call, so the note is normally a no-op — but keeping
            // the pair means a head that detaches during the run still gets the sentence, and
            // a future reader sees one pattern rather than two.
            self.hub
                .note_operator_call(&call_id, &cmd.head_id, &name, &who);
            let said = self.run_operator_call(&call_id, &name, &arguments, &who);
            self.hub.take_operator_call(&call_id);
            if let Err(e) = said {
                // **Said, because there is nothing to return it to.** The worker's path
                // reports a failure as `Outcome::Failed`, which the daemon prints; this one
                // has no caller to answer, so the failure goes on the log or it goes
                // nowhere — and a call that ran with its row unpinned is exactly the kind of
                // silence this tree exists against.
                self.import_note(
                    "transcript_store",
                    format!(
                        "`{who}` ran `{name}` from their own console and the transcript could not be written: {e}. The call ran; what is missing is the record of it, which is what the next turn would have read."
                    ),
                );
            }
        }
    }

    fn apply_queued_mode(&mut self) {
        let Some(cmd) = self.hub.try_mode_command() else {
            return;
        };
        let letibot_sessionlog::hub::CommandKind::Mode { name, consented } = &cmd.kind else {
            return;
        };
        let mode = match letibot_tools::mode::Mode::parse(name) {
            Ok(m) => m,
            Err(e) => {
                self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "mode_unknown".into(),
                    detail: e,

                    compaction: None,
                });
                return;
            }
        };
        // **The rows are written here too** (`modes::record`, the worker's writes). This said the
        // worker's path would write them; it never saw this command, which this turn had already
        // taken off the queue.
        let consented = *consented;
        let moved = self.set_mode_consented(mode, consented);
        // **And written down, as the worker's door writes it** — this door used to apply the
        // point and write nothing, so a `/mode` typed while the session worked was gone at the
        // next restart.
        if let Err(e) = crate::modes::record(
            &self.mode_store,
            self.workspace(),
            &self.cfg.session_id,
            mode,
            consented,
            moved.is_ok(),
        ) {
            self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                code: "mode_unpersisted".into(),
                detail: format!(
                    "`{}` applies to this session now, but could not be recorded, so a restart \
                     will not bring it back: {e}",
                    mode.name
                ),
                compaction: None,
            });
        }
        match moved {
            Ok(said) => {
                let applied = self.cfg.mode;
                self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "mode_set".into(),
                    detail: format!("{said}. {}", applied.summary),

                    compaction: None,
                });
            }
            Err(why) => {
                self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "mode_set_refused".into(),
                    detail: format!("this session stays at `{}`: {why}", self.cfg.mode.name),

                    compaction: None,
                });
            }
        }
    }

    /// **A `/models` the operator typed while this turn was running** — taken and answered here,
    /// on the thread that owns this harness, and **recorded rather than applied**.
    ///
    /// # Why the turn takes it at all
    ///
    /// `Hub::try_command_for_running_turn` carries the measurement (2026-10-12: the
    /// operator's `/models` sat in the queue for a whole 63-second ladder and was
    /// answered after the turn). `/mode` has had this door since R29 and `/models` did not, and
    /// the reply arriving while they are looking matters just as much now that the switch no
    /// longer takes effect mid-turn: the sentence they get back is *what will happen and when*,
    /// which is only useful inside the turn it is about.
    ///
    /// # Why it no longer takes effect here
    ///
    /// Because it must not, and the measurement is [`Harness::set_provider`]'s: a round re-issued
    /// on another model in the middle of a turn carries the rest of the old model's turn, and a
    /// thinking-mode provider refuses exactly that. So the switch goes through the ordinary door,
    /// which records it as pending, and [`Harness::end_turn_clock`] applies it.
    ///
    /// # What it will NOT take
    ///
    /// `slash::mid_turn_switch_ok` decides, and its two `no`s are the switches that
    /// re-seat the conversation — a fleet model's weights, and `local` when this
    /// session is on foreign weights. A re-seat forks the transcript and swaps the
    /// session under `self.session`, under a turn that is in flight with a sink open
    /// and rows still to reconcile. Those lines stay in the queue, in order, and the
    /// worker takes them when the turn ends, exactly as every `/models` did before
    /// this existed: the switch still lands, one turn later than it would have.
    fn apply_queued_model(&mut self) {
        while let Some(cmd) = self.take_queued_model() {
            let letibot_sessionlog::hub::CommandKind::Slash { line } = &cmd.kind else {
                continue;
            };
            let line = line.clone();
            let reply = match crate::slash::Slash::parse(&line) {
                crate::slash::Slash::ModelsSet {
                    provider,
                    model,
                    key,
                } => match crate::slash::models_choice(
                    &provider,
                    model.as_deref(),
                    key.as_deref(),
                    None,
                ) {
                    Ok((choice, lines)) => self.apply_model_choice(choice, lines),
                    Err(lines) => crate::slash::SlashReply { lines, ok: false },
                },
                // The picker only says yes to a model switch, so this is a bug in the
                // predicate rather than a verb. Answered rather than dropped: the
                // command has been taken out of the queue, and a command that vanishes
                // without a word is the failure this tree keeps deleting.
                _ => crate::slash::SlashReply {
                    lines: vec![format!(
                        "/{line}: not a model switch this turn can apply, so nothing moved"
                    )],
                    ok: false,
                },
            };
            // **The reply the worker would have published** — the same two codes and
            // the same body, so "what did my switch do" does not depend on which thread
            // happened to apply it. `apply_queued_mode`'s rule, one verb along.
            self.hub.publish(SessionEvent::Warning {
                code: if reply.ok {
                    "slash".into()
                } else {
                    "slash_refused".into()
                },
                detail: format!("/{line}\n{}", reply.lines.join("\n")),

                compaction: None,
            });
        }
    }

    /// The picker half of [`Harness::apply_queued_model`]: **the next queued command
    /// this running turn can carry out**, with the verb's own grammar answering.
    fn take_queued_model(&self) -> Option<letibot_sessionlog::hub::QueuedCommand> {
        let foreign = self.on_foreign_weights();
        self.hub.try_command_for_running_turn(|kind| match kind {
            letibot_sessionlog::hub::CommandKind::Slash { line } => {
                crate::slash::mid_turn_switch_ok(line, foreign)
            }
            _ => false,
        })
    }

    /// **This attempt failed, and the outage remembers it** — see [`Outage`] for why nothing is
    /// published here.
    ///
    /// **Which host, asked of the thing that was contacted.** This read
    /// `self.cfg.endpoint.authority()` — the LOCAL endpoint — on a path that also serves cloud
    /// turns, so a resolver failure against a provider was reported as a problem with
    /// `127.0.0.1:8080`. MEASURED 2026-10-01: the operator read *"the model server at
    /// 127.0.0.1:8080 did not answer: ... Temporary failure in name resolution"* and went to a
    /// `llama-server` that was answering `/health` with 200. Not knowing which host failed is
    /// what made the message expensive; `cfg.endpoint` is only right for the local route, so the
    /// cloud route is asked for its own.
    fn outage_failed(&mut self, e: &letibot_turn::HttpError) {
        let host = retry_host(self.provider.as_deref(), &self.cfg.endpoint);
        let now = letibot_sessionlog::event::now_ms();
        // **A failure against a DIFFERENT host is a different outage** — the operator's
        // `/models` and a `[fallback]` both move this, and two endpoints' attempts are not one
        // counter. The old record is dropped rather than told: it is over, and the row for it
        // would be about a host this session is no longer failing against.
        match &mut self.outage {
            Some(o) if o.host == host => {
                o.attempts += 1;
                o.last_ms = now;
                o.said = e.to_string();
            }
            slot => {
                *slot = Some(Outage {
                    host,
                    attempts: 1,
                    began_ms: now,
                    last_ms: now,
                    said: e.to_string(),
                });
            }
        }
    }

    /// **The outage is over, and this is the one row it earns** — see [`Outage`].
    ///
    /// `answered` is *the round ended on anything but an HTTP failure*, which includes a round
    /// that failed for some other reason: the endpoint spoke, so the outage is over either way.
    /// The record is TAKEN, so this is the only place that can write the row and it can write it
    /// once.
    fn settle_outage(&mut self, answered: bool) {
        let Some(outage) = self.outage.take() else {
            return;
        };
        // **The endpoint came back, and that is said however few attempts it took** — a silent
        // recovery is how somebody spends an afternoon believing it is still down.
        if answered {
            self.hub.publish(SessionEvent::Warning {
                code: "model_endpoint_answered".into(),
                detail: outage.answered(letibot_sessionlog::event::now_ms()),
                compaction: None,
            });
            return;
        }
        // **One failure the ladder never retried is not an outage**: with `http_retries = 0`
        // the turn's own `turn_failed` is the whole of the news, and a row saying `attempt 1`
        // beside it would be the same fact twice.
        if outage.attempts < 2 {
            return;
        }
        self.hub.publish(SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: outage.did_not_answer(),
            compaction: None,
        });
    }

    /// **The model is out, and this box has somewhere else to go** — `[fallback]
    /// models` in `providers.toml`, tried in the operator's order, once each.
    ///
    /// # It records the move; it does not make it
    ///
    /// The operator's ruling stands — *"it is a standard thing to do - limits, 5xx, etc. we
    /// must be able to change models like for the main session"* — and what changed is WHEN. A
    /// 429 that says `Weekly/Monthly Limit Exhausted` is not weather and no wait lifts it, so the
    /// session moves at the end of the turn instead of inside it; the turn that met the refusal
    /// ends with it.
    ///
    /// **Why the round cannot go on elsewhere** (MEASURED 2026-10-26, [`Harness::set_provider`]
    /// has the whole of it): a re-issued round carries the rest of the old model's turn — its
    /// assistant tool-call rounds — and a thinking-mode provider refuses exactly that, by name.
    /// A model's first request must follow a user message, and the only user message available is
    /// the next turn's.
    ///
    /// # The two guards, and why each is here
    ///
    /// * **`model_refused`**: a 429 or a 5xx only. A connection that was refused, a
    ///   stream that did not parse, a 408 — those are the route rather than the model,
    ///   and moving the session would abandon the model the operator chose for a reason
    ///   that is not about it.
    /// * **`context_length_refusal`**: a provider refusing the prompt for LENGTH is
    ///   answering perfectly well. That is the wall, it is classified as one a few lines
    ///   below in `run_rounds`, and another model would refuse the same prompt.
    ///
    /// # What it will and will not switch to
    ///
    /// A **metered** name only, and this is narrower than `/models` on purpose. A fleet model or
    /// the daemon's own server can re-render the conversation into a fork, and that is not
    /// something a retry loop should do unbidden — not even at a turn boundary, where the fork
    /// would land between the rows the turn has written and the ones it has not. A name it will
    /// not take is SAID and skipped — a fallback nobody can reach, silently, is the defect the
    /// key's own reader is written against.
    ///
    /// Returns the model it recorded, or `None` when the list is empty, spent, or has
    /// nothing this turn can use.
    fn fall_back_from(
        &mut self,
        e: &letibot_turn::HttpError,
        tried: &mut Vec<String>,
    ) -> Option<String> {
        if !model_refused(e) || self.cfg.fallback.is_empty() {
            return None;
        }
        if let letibot_turn::HttpError::Status { body, .. } = e
            && context_length_refusal(body).is_some()
        {
            return None;
        }
        let here = self.model_name();
        for name in self.cfg.fallback.clone() {
            // **Once each, and never the model that just refused the round.** A name
            // already spent is skipped; a name that IS what is answering would be a
            // no-op switch and the same failure one attempt later.
            if tried.contains(&name) || name == here {
                continue;
            }
            tried.push(name.clone());
            let (provider, model) = crate::slash::split_model_name(&name);
            // **The resolver's own notes are dropped on the success arm**, deliberately: a
            // fallback is a switch nobody typed, so the sentence that has to be right is
            // `fallback_notice`'s, said by the caller with the provider's refusal in it.
            let (choice, _notes) =
                match crate::slash::models_choice(&provider, model.as_deref(), None, None) {
                    Ok(x) => x,
                    Err(lines) => {
                        self.fallback_skip(&name, &lines.join(" "));
                        continue;
                    }
                };
            let crate::slash::ModelChoice::Metered(pc) = choice else {
                self.fallback_skip(
                    &name,
                    "a metered model is the only thing an automatic switch may land on, and this \
                     one is the daemon's own server or a fleet model — either can re-render the \
                     conversation into a fork, which is not something the daemon's own switch \
                     should do unbidden",
                );
                continue;
            };
            // **The note the `models_choice` resolver came back with is not carried**: a
            // fallback is a switch nobody typed, so the sentence that matters is
            // `fallback_notice`'s, said by the caller with the provider's own refusal in it.
            let target = model_name_of(&Some(pc.clone()));
            self.pend_switch(Some(pc), target.clone());
            // **And the work goes on by itself** — the operator's ruling of 2026-10-12, and the
            // half a switch alone does not do: *"when it finally applied the turn didnt
            // continue"*. `turn_tail::run_turn` opens this turn when the one that met the refusal
            // has ended, with the notice as its user item.
            //
            // **Not when THIS turn is already such a continuation**: a fallback chain would then
            // ping-pong between two names, paying a model change per turn and never stopping.
            if !self.in_fallback_continuation() {
                self.fallback_continuation = Some(fallback_continuation_notice(&here, &target, e));
            }
            return Some(target);
        }
        None
    }

    /// **A fallback name that could not be taken, said.** The same code as the retry it
    /// is part of — the round is still being taken again, and what this adds is why it
    /// is not going *there*.
    fn fallback_skip(&self, name: &str, why: &str) {
        self.hub.publish(SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: format!(
                "`[fallback]` names `{name}` and this session cannot take it: {why}. \
                 Trying the next name."
            ),

            compaction: None,
        });
    }

    fn steering(&self) -> HubSteering {
        HubSteering {
            hub: self.hub.clone(),
            trail: Some(self.trail.clone()),
            injected: self.injected.clone(),
            monitors: self.monitors.clone(),
            monitor_cursor: self.monitor_cursor.clone(),
            // The session's own set, read by R79's rules: what the work waits on at the
            // round boundary, the held batch only past the cap.
            deliveries: self.deliveries(),
            // **And the session's own children, so a stop takes them with it.** See the
            // interrupt arm of `try_next`.
            subagents: Some(self.subagents.clone()),
            held: self.handed_back.clone(),
            // **And the board, so the operator's answer lands mid-turn.** See the `AnswerTodo` arm.
            todos: Some(self.todos.clone()),
            // **And this session's own parent**, so a message's sentence says which way the edge
            // runs — the same field `submit_a_parents_message` reads, so the two doors word it
            // identically.
            parent: self.cfg.parent_session_id.clone(),
        }
    }

    /// **The intent diff at a turn boundary**, or `None` when there is nothing to
    /// say — which is the common case and is meant to be.
    ///
    /// Two callers into one of two functions, and the difference is a flag:
    ///
    /// * `intent::close_the_turn` reads the assistant's own prose for the
    ///   [`letibot_tools::builtins::intent::commitments`] heuristic **as well as**
    ///   the tool-declared half.
    /// * `IntentLedger::reconcile(turn_id, "")` is the tool-declared half alone,
    ///   which that method's own doc calls *"the deterministic one"*.
    ///
    /// **And it is TWO questions, not one** — both of them *did this turn stop with
    /// something it said it would do still open*. The ledger answers it of the intentions
    /// the turn declared; `todo::unfinished_plan` answers it of the model's OWN plan,
    /// which is the larger half because the plan is where a long session's work actually
    /// lives. Both findings are true and the model should hear both, so they are joined
    /// rather than one being chosen over the other; either alone is the message.
    ///
    /// The deterministic half is the default because of the rule this whole strand
    /// is under: **nothing widens by default.** The prose heuristic can only fire on
    /// a turn that ran nothing at all, which for a plain answer is the normal case,
    /// so switching it on for every existing session would put *"you said you would
    /// X"* into conversations that were working. It is the more useful half and it
    /// is the one with false positives, so it is `--intent-prose` and its cost is
    /// written where somebody deciding can read it.
    ///
    /// The deterministic half needs no flag because it cannot fire unless the model
    /// used `todo` or `goal`, and those are seated only by a role that names them.
    /// A default `letibot` session is behaviourally identical.
    fn close_the_turn(&self, turn_id: &str, items: &[TranscriptItem]) -> Option<String> {
        // **THE PLAN IS NOT CHECKED HERE ANY MORE, and that is the scheduling change.**
        //
        // The turn-boundary check is for a claim about the turn that just ended — T21.3's *"when
        // model says ill start that and by end of the turn forgets and does not start anything"* —
        // and the intent ledger is exactly that (`steer_for_turn`).
        //
        // The PLAN is not a claim about one turn; it is a standing fact about the session, and
        // checking it at every turn boundary gave the operator both failure modes at once: a
        // session in constant conversation was checked after every exchange (*"looks like our todo
        // nag is overkill"*), while a session that went quiet was checked once and then never
        // again. It is now delivered by the IDLE clock instead — `Sessions::rearm_todo_nag` and
        // `Harness::nag_turn`, on the operator's own instruction: *"maybe wait for a timeout
        // actually. so send it when model is idling"* and *"but certainly not after my message."*
        //
        // One check, one schedule. `None` here is what makes that true rather than a comment
        // saying so, and `close_the_turn_with` keeps the plan parameter so the join it performs is
        // still reachable from a test.
        self.close_the_turn_with(turn_id, items)
    }

    /// The intent finding for a turn that has just ended.
    ///
    /// A named function rather than the call inlined above, because it is the seam the tests
    /// reach — and it is now the WHOLE of the turn-boundary check. The plan's finding used to be
    /// joined in here (see `close_the_turn` for why it is not), so a reader looking for where the
    /// todo check happens will not find it at this layer at all: it is `Harness::nag_turn`, on the
    /// idle clock.
    fn close_the_turn_with(&self, turn_id: &str, items: &[TranscriptItem]) -> Option<String> {
        // R93.F30: one plan tool per seat. Where it is `todo_write`, the intent ledger's own
        // board is empty and the check reads the `todo_write` board instead.
        let seated = tool_names(&self.prefix.tools_json);
        let plan = (seated.contains("todo_write") && !seated.contains("todo"))
            .then(|| intent_tools::ledger::Board::of_plan(&self.todos.snapshot()));
        steer_for_turn_over(&self.intent, self.cfg.intent_prose, turn_id, items, plan)
    }

    /// **One round's context claim, judged, logged and — for the operator — warned
    /// about.** The line for the MODEL is returned, for the caller to deliver where the
    /// round's shape says it goes (a notice after a turn that ended, the step boundary
    /// for one that is still calling tools). See `crate::context_claim`.
    fn judge_context_claim(
        &mut self,
        check: &mut crate::context_claim::Check,
        text: &str,
        reasoning: bool,
    ) -> Option<String> {
        use crate::context_claim::{REPEATED, Verdict};
        if text.trim().is_empty() {
            return None;
        }
        let fill = self.context_fill();
        let mut finding = match reasoning {
            false => check.judge(text, fill)?,
            true => check.judge_reasoning(text, fill)?,
        };
        // R93.A4: with auto-compaction off, "the harness handles it" is false — nothing compacts
        // at the wall (`turn_tail` says so when the turn stops there) — so the model is told
        // nothing, and the operator is not told it was. The claim is still logged and measured.
        if !self.cfg.auto_compact {
            finding.verdict = Verdict::Quiet;
        }
        finding.log(&self.cfg.daemon_logs(), &self.cfg.session_id);
        crate::instruments::context_claim(&self.cfg.session_id, &finding);
        match finding.verdict {
            Verdict::Slap(line) => Some(line),
            Verdict::Repeated(detail) => {
                self.hub.publish(SessionEvent::Warning {
                    code: REPEATED.into(),
                    detail,
                    compaction: None,
                });
                None
            }
            Verdict::Quiet => None,
        }
    }

    /// **The session's fill, as the wall judges it** — the one place a context claim's
    /// measurement is read, so the day the wall's measurement changes (R72 rewrites
    /// `planning_window` and the ledger scale) this changes with it and nothing else
    /// does.
    ///
    /// The verdict is the wall's own: `Config::at_the_wall` over the ledger, the
    /// predicate `compact_at_the_wall` acts on — so the check never calls a claim false
    /// that the harness would have compacted for. The two numbers the model is told are
    /// in the units the head shows (`Config::shown_tokens`), because a figure the
    /// operator cannot find on their own screen is a second disagreement. `None` when the
    /// window is unknown: not knowing is not a measurement, and a claim cannot be
    /// checked against it.
    fn context_fill(&mut self) -> Option<crate::context_claim::Fill> {
        // **R72's count, the one the head and the wall show.** `measure_resident` refreshes
        // `cfg.ledger_scale` from the provider's own last count plus what was sent since, so
        // `planning_window`, `at_the_wall` and `shown_tokens` below all read that one number —
        // the slap quotes the fill the operator sees, not the ledger's reasoning-heavy length.
        self.measure_resident();
        self.cfg.planning_window()?;
        let resident = self.session.ledger.len() as u64;
        Some(crate::context_claim::Fill {
            resident: self.cfg.shown_tokens(resident),
            // **The window as it was GIVEN** (`shown_wall`, R72's rule for a line), not
            // `shown_tokens(planning_window())` — a round trip through the ratio, which is where
            // every row of R71's log got `999999` for a 1,000,000-token window (TODO R81).
            window: self.cfg.shown_wall().0,
            at_wall: self.cfg.at_the_wall(resident),
        })
    }

    /// The authorisation trail this session would show an adjudicator right now.
    ///
    /// Public because it is the input to a decision taken on the operator's behalf,
    /// and §4c wants the trail stored as **what was actually shown to the model**
    /// rather than as a later reconstruction — which means something has to be able
    /// to read it without going through a denial.
    pub fn trail(&self) -> AuthorisationTrail {
        self.trail.trail()
    }

    /// The intent ledger, for a head that wants to draw the board.
    pub fn intent(&self) -> &Arc<IntentLedger> {
        &self.intent
    }

    /// **Tell this session's model something the HARNESS found, without running a turn.**
    ///
    /// The door [`crate::plugins`] needs and the reason it queues rather than publishes: a layer
    /// the shadow gate refused, or a pane that answered something the contract refuses, is a fact
    /// the MODEL is the one that can fix — and a fact only the harness can put in front of it,
    /// because a notice is a transcript row and rows are the harness's to write. The tick that
    /// discovers it runs on the worker's idle pass, where the harness is in hand; the gate itself
    /// runs off that pass, on a thread, which is why it hands the sentence over instead of
    /// speaking ([`crate::plugins::PluginHost::take_notices`]).
    ///
    /// It is `append_notice` and not a turn: a plugin that failed to load is not a reason to
    /// spend a generation, and the row is read by the model's next turn like any other harness
    /// note.
    pub(crate) fn note_to_model(&mut self, text: &str) -> Result<(), HarnessError> {
        // **Not through the `notice` hook, on purpose.** This is a plugin layer's own failure — a
        // gate refusal, a pane the contract refused — and a layer that traps inside the hook that
        // reports trapping would be a loop with a model at the end of it. §4.3's rule is that the
        // harness's notices may be reworded; a notice about a broken report is not one of them.
        self.record_notice(text)
    }

    /// **One notice through the plugin stack** — the sentence to record, or `None` when a layer
    /// dropped it (`docs/plugins-plan.md` §4.3).
    ///
    /// A session with no layers, or one whose layers answer other hooks, gets its sentence back
    /// unchanged and pays one walk over an empty slice: `PluginHost::notice` answers before it
    /// builds an input when nothing exports `notice_<kind>`, which is what makes *a session with
    /// no layers behaves as it did* a property of the code rather than a promise.
    fn noticed(&self, kind: &str, text: &str, facts: serde_json::Value) -> Option<String> {
        match self.plugin_host() {
            Some(host) => host.notice(&self.hub, kind, text, facts),
            None => Some(text.to_string()),
        }
    }

    /// [`Harness::noticed`] over the batch a wake composed from one source — the queue's sentences,
    /// the monitor firings, the jobs that settled.
    fn noticed_all(&self, kind: &str, lines: Vec<String>, facts: serde_json::Value) -> Vec<String> {
        if lines.is_empty() {
            return lines;
        }
        lines
            .into_iter()
            .filter_map(|line| self.noticed(kind, &line, facts.clone()))
            .collect()
    }

    /// **A notice this harness injects, said once and dropped if a layer dropped it.**
    ///
    /// The kinds are `letibot_plugin::notice`'s; the sentence is the site's own.
    fn append_notice(&mut self, kind: &str, text: &str) -> Result<(), HarnessError> {
        let Some(text) = self.noticed(kind, text, serde_json::json!({})) else {
            return Ok(());
        };
        self.record_notice(&text)
    }

    /// The row itself, whoever decided the sentence.
    fn record_notice(&mut self, text: &str) -> Result<(), HarnessError> {
        // The harness talking to itself. Recorded as such, so a §5.7 salvage notice
        // or an intent nudge can never be read back as the operator authorising the
        // action it is about — which is the whole reason [`Speaker`] has three
        // values rather than one.
        self.trail.say(Speaker::Agent, text, Some(Instant::now()));
        let item = TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text: text.into() }],
        };
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session
            .append_items(&self.engine, std::slice::from_ref(&item), &mut sink)?;
        self.reconcile(&mut sink, std::slice::from_ref(&item));
        self.persist()
    }

    /// **Say the calls the gate admitted without asking anybody** (D16), one transcript row
    /// each.
    ///
    /// Called at the round boundary — after a round's results are on the log and before the
    /// model generates again — because that is the one point in a turn where a row can be
    /// appended without landing between a tool call and its result. `pair_tool_calls` reads
    /// the first non-`tool` row as the end of an answering window, so a notice written
    /// between them would make the model read *no result was recorded* beside the real one.
    ///
    /// A session whose gate has no admission sink (no adjudicator, or a fixture) has an empty
    /// queue and pays one lock; a session at a point that asks nothing of this kind never
    /// queues anything at all.
    fn flush_gate_admissions(&mut self) -> Result<(), HarnessError> {
        let lines: Vec<String> = {
            let mut q = self
                .gate_admissions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *q)
        };
        for line in lines {
            self.append_notice("gate", &line)?;
        }
        Ok(())
    }

    /// Attach content to the rows the log already announced (T13.1).
    ///
    /// The ids come off the events rather than being recomputed; see point 1 in the
    /// module header. A count mismatch is a bug in the pairing, not something to
    /// paper over, so it is announced as a warning on the same log the head reads.
    fn reconcile(&self, sink: &mut CapturingSink, items: &[TranscriptItem]) {
        // **What this session now holds** (R66). Restated here because this is the one function
        // every append goes through — the six `append_items` call sites and the compaction
        // fork's swap — so the census's number is a walk of the transcript as it is, and not a
        // counter that can drift from it. See [`crate::footprint`].
        self.restate_footprint();
        // The trail's denominator, counted where every appended batch passes.
        // `messages_scanned` has to be what was **looked at**, and this is the one
        // place that knows how much there is.
        self.trail.note_items(items.len());
        let ids = reconcile_rows(&self.hub, sink, items);
        // **And what each reply cited**, read now — the moment it landed.
        for (id, item) in ids.iter().zip(items) {
            self.publish_citations(id, item);
        }
    }

    /// **The lines a reply cites, published after its row** ([`crate::citations`]). The stored
    /// snapshot when there is one — a reply restored or carried by a fork shows what it cited
    /// when it was written — and otherwise the file as it is now, read through this session's
    /// backend and stored. Silent for a row that cites nothing.
    pub(crate) fn publish_citations(&self, item_id: &str, item: &TranscriptItem) {
        let TranscriptItem::Assistant { text, .. } = item else {
            return;
        };
        let cited = crate::citations::cited_fences(text);
        if cited.is_empty() {
            return;
        }
        let hash = hex32(&Sha256::digest(text.as_bytes()).into());
        let session = &self.cfg.session_id;
        let stored = self
            .store
            .as_ref()
            .and_then(|s| s.citations(session, &hash).ok().flatten())
            .and_then(|j| serde_json::from_str::<Vec<letibot_sessionlog::Citation>>(&j).ok());
        let citations = match stored {
            Some(c) => c,
            None => {
                let backend = &self.runtime.backend;
                let read = |p: &str| backend.read(p).map_err(|e| e.to_string());
                let fresh: Vec<letibot_sessionlog::Citation> = cited
                    .iter()
                    .map(|c| crate::citations::snapshot(c, read))
                    .collect();
                if let Some(s) = self.store.as_ref()
                    && let Ok(json) = serde_json::to_string(&fresh)
                    && let Err(e) = s.put_citations(
                        session,
                        &hash,
                        &json,
                        letibot_sessionlog::registry::now_ms(),
                    )
                {
                    eprintln!("  {session} · a reply's citations were not stored: {e}");
                }
                fresh
            }
        };
        self.hub
            .publish(letibot_sessionlog::SessionEvent::Citations {
                item_id: item_id.to_string(),
                citations,
            });
    }

    /// **Run the summary turn and pair every row it appended** — the one door both
    /// callers go through, because the two of them forgetting is exactly the defect
    /// this closes.
    ///
    /// `run_compaction` appends the summary INSTRUCTION and the model's ANSWER to the
    /// session through the sink, and a sink announces each row it is handed.
    /// `reconcile` is the only thing that hands the hub the row's BODY. Two callers
    /// (`compact_inner`'s ordinary arm and `reseat`) ran `run_compaction` against this
    /// session's own sink and never reconciled it — the ids stayed in the sink and
    /// were dropped with it, so the two rows were announced and never filled.
    ///
    /// **The hole was in the DAEMON's view, so nothing on a head could repair it**:
    /// every later snapshot announced the same two rows with `item: None`, and
    /// `/resync` serves that same view — which is why the operator ran it three times
    /// and the `2 row(s) announced … and never filled` line did not move. MEASURED on
    /// their live session 2026-10-05: rows `#t45.510` (the instruction) and `#t45.511`
    /// (the summary), both stored with their bodies and both served body-less, until
    /// the daemon was restarted and the resume's republish filled them.
    ///
    /// The slice is taken from the session AFTER the turn, so the pairing is the
    /// session's own order — the same positional zip `reconcile` documents, and it
    /// covers the salvage notices a say-nothing summary turn appends as well as the
    /// instruction and the answer.
    ///
    /// An associated function rather than a method, and the reason is the borrow:
    /// `answerer` already holds `&self.provider` and `&self.prefix` for the call, so
    /// a `&self` receiver here would overlap it. `hub` is passed in for the same
    /// reason the row pairing is a free function.
    fn run_summary_turn(
        hub: &Arc<Hub>,
        engine: &mut TurnEngine,
        session: &mut Session,
        sink: &mut CapturingSink,
        answerer: &letibot_turn::compaction::Answerer<'_>,
        ladder: &mut letibot_http::retry::Ladder,
    ) -> Result<CompactionOutcome, HarnessError> {
        let mark = session.items.len();
        // **The summary turn is a provider call, so it walks the same ladder** — and its
        // failure is said the same way, with the count in it. A compaction that failed on a
        // blip used to end the session's only chance to fold, and the operator read
        // `compaction FAILED: http io: …` with no way to tell one attempt from six.
        let outcome =
            run_compaction(engine, session, sink, answerer, ladder).map_err(|e| match e {
                TurnFailure::Http(e) => HarnessError::Endpoint {
                    said: ladder.ending(&e),
                    cause: e,
                },
                other => HarnessError::Turn(other),
            })?;
        let added: Vec<TranscriptItem> = session.items[mark..].to_vec();
        reconcile_rows(hub, sink, &added);
        Ok(outcome)
    }

    /// Write every ledger row that has not reached the store yet.
    ///
    /// Persist and announce the todo list, if the model wrote one since last time.
    ///
    /// The tool mutates the board; this is the harness noticing. Called at every
    /// round boundary so the pane moves with the work rather than after it — a
    /// plan that reaches the head only when the turn ends is a plan the operator
    /// watches being executed blind. A storeless session announces but does not
    /// persist: the list is still real for the life of the session, and saying so
    /// once in the open notes would be the honest form; silently pretending it
    /// persisted would not be.
    /// **[`Harness::flush_todos`] for a caller with no error to return** — the three doors that
    /// used to `eprintln!` the failure and nothing else (R67 item 15): the operator's own rows,
    /// the review queue's board, and a fired condition being spent. Each is a plan the store does
    /// not hold, and the last is the worst of them: a condition that fired and was not written
    /// down fires AGAIN after a restart. So the failure reaches the session's hub as
    /// `todos_not_stored`, beside the daemon's log line.
    fn flush_todos_or_say(&mut self, what: &str) {
        if let Err(e) = self.flush_todos() {
            eprintln!("  todos: {what} could not be written down: {e}");
            self.import_note(
                "todos_not_stored",
                format!(
                    "the plan moved ({what}) and the store did not take it: {e}. The board you \
                     see is right for this daemon's life; after a restart it comes back as the \
                     store last held it."
                ),
            );
        }
    }

    fn flush_todos(&mut self) -> Result<(), HarnessError> {
        let v = self.todos.version();
        if v == self.todos_version {
            return Ok(());
        }
        let items = self.todos.snapshot();
        // **Patient (R91.6), and only then marked written (R91.7).** The version was recorded
        // BEFORE the write, so a write that failed left the next flush seeing "nothing moved"
        // and returning `Ok` without writing — and a restart then reverted the operator's
        // answers, `wait_on` edges and consumed conditions. Now a busy store is waited out as
        // the transcript's rows are, and the version moves only once the store has the board.
        if self.store.is_some() {
            let session = self.cfg.session_id.clone();
            self.store_patiently("todos", |store| store.put_todos(&session, &items))?;
        }
        self.todos_version = v;
        // **AND SAY IT LOUDLY WHEN A PERSON STARTS OR STOPS BEING WAITED ON** — *"the heads show
        // loud notification that im waited"*. Compared across flushes, so a re-publish of the same
        // board says nothing and each question is announced once when it is asked and once when it
        // is settled (answered, detached by the model, or its row closed). The settings row moves
        // with it, because that is what a head attaching later reads.
        let waiting = crate::operator_ask::waiting(&items);
        if waiting != self.asks_said {
            for (row, question) in waiting.iter().filter(|w| !self.asks_said.contains(w)) {
                self.hub.publish(SessionEvent::Warning {
                    code: "waiting_on_operator".into(),
                    detail: crate::operator_ask::asked_words(row, question),
                    compaction: None,
                });
            }
            for (row, question) in self.asks_said.iter().filter(|w| !waiting.contains(w)) {
                self.hub.publish(SessionEvent::Warning {
                    code: "operator_question_settled".into(),
                    // **And WHY it stopped** — an answer, a take-back and a row that closed are
                    // three different facts, and the person who was asked can only tell their own
                    // answer landed if the notice says so. See `operator_ask::settled_words`.
                    detail: crate::operator_ask::settled_words(&items, row, question),
                    compaction: None,
                });
            }
            self.asks_said = waiting;
            self.publish_settings();
        }
        self.hub.publish(SessionEvent::TodosUpdated {
            todos: items.into_iter().map(Self::todo_entry).collect(),
        });
        Ok(())
    }

    /// **The operator's answer to a row's question, between turns** — applied to the board,
    /// persisted and announced, and the line the model must be told handed back for the turn the
    /// caller runs with it (`Sessions::dispatch`'s `AnswerTodo` arm, and a child's own thread).
    /// The mid-turn half is `HubSteering`'s arm; both go through [`crate::operator_ask::settle`].
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

    /// The store's todo shape, as the wire spells it. Two types because the two
    /// crates cannot share one — the protocol does not grow a storage dependency
    /// to save a `struct` — and one conversion because the fields are the same
    /// four words.
    pub(crate) fn todo_entry(t: TodoItem) -> TodoEntry {
        TodoEntry {
            content: t.content,
            // **The store's condition, as the WIRE spells it.** A `match` rather than a
            // pass-through because the two crates keep their own copy of the type on purpose,
            // so a variant has to be spelled on both sides — which is what makes an
            // unevaluable condition a compile error instead of a row that waits for ever.
            when: t.when.map(|c| match c {
                letibot_tokencore::store::TodoCondition::Job { handle } => {
                    letibot_sessionlog::event::TodoCondition::Job { handle }
                }
            }),
            // **And its EDGES, by the same rule and for a second reason.** The `match` is the
            // compile-time half (a need kind this head cannot decode would be a row nobody could
            // act on); the field itself is what makes the pane able to CARRY an edge rather than
            // only draw one — see `TodoEntry::needs`.
            needs: t
                .needs
                .into_iter()
                .map(|need| match need {
                    letibot_tokencore::store::TodoNeed::Row { content } => {
                        letibot_sessionlog::event::TodoNeed::Row { content }
                    }
                    // **The child's id travels verbatim**, the same rule `TodoBy::Parent`'s author
                    // string keeps: it is the handle `task_result` collects by, so a head that
                    // shortened it would hand the model a name nothing answers to.
                    letibot_tokencore::store::TodoNeed::Child { id } => {
                        letibot_sessionlog::event::TodoNeed::Child { id }
                    }
                    // **And the job handle travels verbatim too**, for the same reason one layer
                    // down: it is the name `job_output` and `job_kill` are given, and the pane has
                    // to be able to carry an edge into a job rather than only draw one.
                    letibot_tokencore::store::TodoNeed::Job { handle } => {
                        letibot_sessionlog::event::TodoNeed::Job { handle }
                    }
                    letibot_tokencore::store::TodoNeed::Monitor { name } => {
                        letibot_sessionlog::event::TodoNeed::Monitor { name }
                    }
                    // **The room's item number crosses verbatim**, the same rule the child's id and
                    // the job handle keep: it is what `room`'s `done`, `beat` and `abandon` take,
                    // and the head draws the edge so a person can see the row is quiet on purpose.
                    letibot_tokencore::store::TodoNeed::Room { item, working } => {
                        letibot_sessionlog::event::TodoNeed::Room { item, working }
                    }
                    // **An unknown kind stays unknown on the wire**, which is what lets a head
                    // that does not know one pass it back to a daemon that does rather than
                    // dropping the edge it cannot read.
                    // **The operator's question travels whole** — the options are what the head's
                    // card offers, and an answer is checked against them on the way back.
                    letibot_tokencore::store::TodoNeed::Operator { question, options } => {
                        letibot_sessionlog::event::TodoNeed::Operator { question, options }
                    }
                    letibot_tokencore::store::TodoNeed::Answered { question, reply } => {
                        letibot_sessionlog::event::TodoNeed::Answered { question, reply }
                    }
                    letibot_tokencore::store::TodoNeed::Unknown => {
                        letibot_sessionlog::event::TodoNeed::Unknown
                    }
                })
                .collect(),
            by: match t.by {
                letibot_tokencore::store::TodoBy::Model => letibot_sessionlog::event::TodoBy::Model,
                letibot_tokencore::store::TodoBy::Operator => {
                    letibot_sessionlog::event::TodoBy::Operator
                }
                // **The author string TRAVELS**: `Parent s-…` is the same string in the store and
                // on the wire, which is what lets a head or a `sqlite3` reader name the session
                // that wrote a child's row without a second lookup.
                letibot_tokencore::store::TodoBy::Parent(who) => {
                    letibot_sessionlog::event::TodoBy::Parent(who)
                }
                // **The queue's own word, and it is the same word on both sides** — the fourth
                // author is a name and not a string, so there is nothing to carry. A head that
                // predates it cannot decode it, which is what moved `PROTOCOL_VERSION` to 42.
                letibot_tokencore::store::TodoBy::Queue => letibot_sessionlog::event::TodoBy::Queue,
            },
            status: match t.status {
                letibot_tokencore::store::TodoStatus::Pending => WireTodoStatus::Pending,
                letibot_tokencore::store::TodoStatus::InProgress => WireTodoStatus::InProgress,
                letibot_tokencore::store::TodoStatus::Completed => WireTodoStatus::Completed,
                letibot_tokencore::store::TodoStatus::Postponed => WireTodoStatus::Postponed,
                letibot_tokencore::store::TodoStatus::Cancelled => WireTodoStatus::Cancelled,
            },
        }
    }

    /// The database enforces the append-only property itself — a trigger refuses a
    /// `seq` that is not the next one and a `tok_offset` that does not continue the
    /// previous row — so a divergence between the ledger and the store is a failed
    /// INSERT rather than a quiet inconsistency.
    /// Wait, unless this session is going away. `false` means it is: the hub
    /// closed under us and there is nobody left to answer.
    ///
    /// Sliced rather than one `sleep`, because the longest wait here is
    /// thirty-two seconds and a daemon asked to stop during one should not have
    /// to sit it out — `Ctrl+C Ctrl+C` closes the registry from the connection's
    /// thread, and this is the worker noticing.
    fn sleep_unless_closed(&self, total: std::time::Duration) -> bool {
        let slice = std::time::Duration::from_millis(250);
        let deadline = std::time::Instant::now() + total;
        while std::time::Instant::now() < deadline {
            if self.hub.is_closed() {
                return false;
            }
            std::thread::sleep(slice.min(deadline - std::time::Instant::now()));
        }
        !self.hub.is_closed()
    }

    fn persist(&mut self) -> Result<(), HarnessError> {
        self.persist_reporting(&mut |_, _| {})
    }

    /// **One store write, with the patience [`Self::persist_reporting`] has** (R91.6): the
    /// store's own minute of busy retries, and past that a wait — said on the session's log
    /// every two minutes — until the store frees, the session closes, or
    /// [`STORE_BUSY_PATIENCE`] runs out counted from the first busy failure. Anything that is
    /// not busy is returned at once. `f` must be safe to run again: a busy write committed
    /// nothing.
    fn store_patiently<T>(
        &self,
        what: &str,
        mut f: impl FnMut(&Store) -> letibot_tokencore::store::Result<T>,
    ) -> Result<T, HarnessError> {
        let Some(store) = &self.store else {
            return Err(HarnessError::Store(format!(
                "{what}: this session has no store"
            )));
        };
        let mut first_busy: Option<std::time::Instant> = None;
        let mut said_at: Option<std::time::Instant> = None;
        loop {
            match f(store) {
                Ok(t) => return Ok(t),
                Err(e) if e.is_busy() => {
                    let since = *first_busy.get_or_insert_with(std::time::Instant::now);
                    let waited = since.elapsed();
                    if waited >= STORE_BUSY_PATIENCE {
                        return Err(HarnessError::Store(format!(
                            "{what}: {e} — still busy after waiting {}s",
                            waited.as_secs()
                        )));
                    }
                    if said_at.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(120)) {
                        said_at = Some(std::time::Instant::now());
                        self.hub.publish(SessionEvent::Warning {
                            code: "store_busy_wait".into(),
                            detail: format!(
                                "the session store is busy (`{e}`) — another daemon holds its \
                                 write lock. The {what} write is kept in memory and made when it \
                                 frees; this session is waiting, not failing ({}s so far).",
                                waited.as_secs()
                            ),
                            compaction: None,
                        });
                    }
                    if !self.sleep_unless_closed(std::time::Duration::from_secs(5)) {
                        return Err(HarnessError::Store(format!(
                            "{what}: {e} — the session closed while waiting for the store"
                        )));
                    }
                }
                Err(e) => return Err(HarnessError::Store(format!("{what}: {e}"))),
            }
        }
    }

    /// **The rows not yet written, written in chunks of [`PERSIST_CHUNK`], one transaction
    /// each** — and `progress(done, total)` told after every chunk, in rows of this call.
    ///
    /// It used to be one autocommit `INSERT` per row, which is free for the two or three rows
    /// a round appends and ruinous for a fork that carries the whole conversation: the rano
    /// session's `/reseat`, 2026-10-12, spent 6.5 minutes writing 971 rows that took two
    /// seconds to render. See [`letibot_tokencore::store::Store::append_items`] for where the
    /// time went. Chunked rather than one transaction for the lot so the write lock every
    /// daemon on the box shares is held for a bounded stretch at a time, and so a long carry
    /// can say how far it has got between chunks.
    ///
    /// `persisted` advances only after a chunk has committed, so a chunk that fails leaves
    /// the count at the last row that is really in the store, and the next call retries from
    /// there.
    ///
    /// **A busy store is waited out here, not failed on** (`STORE_BUSY_PATIENCE`): the store
    /// retries a busy chunk for a minute itself, and past that this loop resyncs `persisted`
    /// from the store, says `store_busy_wait` on the session's hub at most every two minutes,
    /// and tries again — giving up only when the session closes or the patience runs out.
    fn persist_reporting(
        &mut self,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), HarnessError> {
        let Some(store) = &self.store else {
            self.persisted = self.session.ledger.rows().len();
            return Ok(());
        };
        let rows = self.session.ledger.rows();
        let start = self.persisted;
        let total = rows.len().saturating_sub(start) as u64;
        let started = std::time::Instant::now();
        let mut said_at: Option<std::time::Instant> = None;
        while self.persisted < rows.len() {
            let from = self.persisted;
            let to = (from + PERSIST_CHUNK).min(rows.len());
            let mut chunk: Vec<(&TranscriptItem, &letibot_tokencore::LedgerRow, &[_])> =
                Vec::with_capacity(to - from);
            for (i, row) in rows.iter().enumerate().take(to).skip(from) {
                let item = self.session.items.get(i).ok_or_else(|| {
                    HarnessError::Store(format!(
                        "ledger row {i} ({}) has no item; the two are supposed to be \
                         appended together",
                        row.item_id
                    ))
                })?;
                let tokens = self
                    .session
                    .ledger
                    .item_tokens(i)
                    .ok_or_else(|| HarnessError::Store(format!("no tokens for row {i}")))?;
                chunk.push((item, row, tokens));
            }
            match store.append_items(&self.transcript_id, from as u32, chunk) {
                Ok(()) => {
                    self.persisted = to;
                    progress((to - start) as u64, total);
                }
                // **A busy store is waited for, never died of.** pg-noop's model, 2026-10-12
                // ~01:20: *"the child died on sqlite: database is locked in the harness's own
                // store … both cost an agent's worth of work."* The store has already retried
                // for a minute on top of its busy timeout; past that the turn WAITS, says so on
                // the session's own log, and only gives up when the session is going away or
                // the wait is past `STORE_BUSY_PATIENCE` — a store wedged for half an hour is a
                // fault to report, not a contention to sit out.
                Err(e) if e.is_busy() => {
                    // A chunk is one transaction and a busy one committed nothing, but the
                    // store is the authority: resume from what it holds, or the next attempt
                    // re-sends a row and the append-only trigger refuses it.
                    if let Ok(n) = store.item_count(&self.transcript_id) {
                        self.persisted = (n as usize).clamp(from, rows.len());
                    }
                    let at = self.persisted;
                    let waited = started.elapsed();
                    if waited >= STORE_BUSY_PATIENCE {
                        return Err(HarnessError::Store(format!(
                            "row {at}: {e} — still busy after waiting {}s; nothing from row \
                             {at} on is stored",
                            waited.as_secs()
                        )));
                    }
                    if said_at.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(120)) {
                        said_at = Some(std::time::Instant::now());
                        self.hub.publish(SessionEvent::Warning {
                            code: "store_busy_wait".into(),
                            detail: format!(
                                "the session store is busy (`{e}`) — another daemon holds its \
                                 write lock. Rows from {at} on are kept in memory and written \
                                 when it frees; this session is waiting, not failing \
                                 ({}s so far).",
                                waited.as_secs()
                            ),
                            compaction: None,
                        });
                    }
                    if !self.sleep_unless_closed(std::time::Duration::from_secs(5)) {
                        return Err(HarnessError::Store(format!(
                            "row {at}: {e} — the session closed while waiting for the store"
                        )));
                    }
                }
                Err(e) => {
                    if let Ok(n) = store.item_count(&self.transcript_id) {
                        self.persisted = (n as usize).clamp(from, rows.len());
                    }
                    return Err(HarnessError::Store(format!(
                        "rows {from}..{to} ({}..): {e}",
                        rows[from].item_id
                    )));
                }
            }
        }
        self.seal_fork();
        Ok(())
    }

    /// The round's prompt size onto the session row. See the call in `run_rounds`
    /// for why this is the number a head shows as the session's context.
    fn persist_context(&self, metrics: &TurnMetrics, rendered_at: u64) -> Result<(), HarnessError> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store
            .set_context(
                &self.cfg.session_id,
                Some(metrics.prompt_tokens),
                Some(metrics.cached_tokens),
                // **The same ledger the in-memory anchor above just used**, so a
                // restart recovers what this daemon had rather than a second
                // opinion about it: the ledger length of the PROMPT the count
                // measured (R72), which the round's reply was appended after. A
                // restart adds those rows back as `recovered_scale` describes.
                Some(rendered_at),
            )
            .map_err(|e| HarnessError::Store(format!("context: {e}")))
    }

    /// **THE SWITCH, ON THE SESSION ROW** — so a restart brings the session back on
    /// what it was switched to rather than on the daemon's own default.
    ///
    /// Called from the `/models` arm the moment the in-memory switch has taken, the
    /// same place `publish_settings` is: the two are one act's two halves, and a
    /// switch that reached the screen but not the row was a choice that expired with
    /// the process. `local` is stored BY NAME rather than left NULL, because the two
    /// answer different questions at a resume — *no opinion* against *the local
    /// server, deliberately* — and a daemon whose own default is a provider would
    /// otherwise hand a switched-back session straight back to it.
    pub fn persist_provider_choice(&self) -> std::result::Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let stored = self.model_name();
        store
            .set_provider_choice(&self.cfg.session_id, Some(&stored))
            .map_err(|e| format!("the choice was not written to the session row: {e}"))
    }

    /// **A RESUME RESTORES THE SESSION'S OWN MODEL**, from the row the switch wrote.
    ///
    /// The defect, in one sentence: a session switched to a cloud provider came back
    /// on the CLI default after a restart, because `set_provider` was in-memory and
    /// the session row had nowhere to record the choice. Called at both open paths —
    /// the first session a `--continue` resumes and the lazy `harness()` a head's
    /// switch reaches — before any turn runs.
    ///
    /// Three spellings of the row, three behaviours, and the distinction is the
    /// point:
    ///
    /// * `None` — never switched, so the daemon's own default stands, which is what
    ///   such a session always did;
    /// * `local` — the deliberate switch back, restored as an ACT (`set_provider(None)`)
    ///   rather than skipped, because a daemon whose default is a provider would
    ///   otherwise hand the session straight back to it;
    /// * `name` or `name/model` — resolved through the SAME door the switch went
    ///   through (`models_choice`), so a restore cannot build a provider the verb
    ///   would refuse. A preset this build no longer knows fails the restore and the
    ///   daemon default stands — said rather than silent, and the row is left alone so
    ///   a build that does know the preset still finds it.
    ///
    /// Returns the sentence a resume should say, or `None` when there was nothing to
    /// restore.
    pub fn restore_provider_choice(&mut self) -> Option<String> {
        let stored = self
            .store
            .as_ref()
            .and_then(|s| s.provider_choice(&self.cfg.session_id).ok())
            .flatten()?;
        if stored == "local" {
            self.cfg.provider.as_ref()?;
            return match self.set_provider(None) {
                Ok(_) => Some("restored to the local server".into()),
                Err(e) => Some(format!("could not restore the local server: {e}")),
            };
        }
        let (name, model) = match stored.split_once('/') {
            Some((n, m)) => (n.to_string(), Some(m.to_string())),
            None => (stored.clone(), None),
        };
        // Already there: a live session's second caller (a head switching IN, not a
        // resume) must not re-switch it and repaint the settings.
        let already = self
            .cfg
            .provider
            .as_ref()
            .is_some_and(|pc| pc.name == name && pc.model == model);
        if already {
            return None;
        }
        match crate::slash::models_choice(&name, model.as_deref(), None, None) {
            Ok((crate::slash::ModelChoice::Metered(choice), _)) => {
                match self.set_provider(Some(choice)) {
                    Ok(_) => Some(format!("restored to {stored}")),
                    Err(e) => Some(format!("could not restore {stored}: {e}")),
                }
            }
            // **A fleet model is restored through its own door**, which re-checks the
            // vocabulary. That check is not ceremony on a resume: the daemon being
            // resumed INTO may have been started on different weights than the one
            // that stored the row, and a restore that skipped it would be the one
            // path into exactly the corruption `set_local_model` exists to refuse.
            Ok((crate::slash::ModelChoice::Local(m), _)) => match self.set_local_model(&m) {
                Ok(_) => Some(format!("restored to {stored}")),
                Err(e) => Some(format!("could not restore {stored}: {e}")),
            },
            // `ModelChoice::Local` is `models_choice("local")`, and `local` is handled
            // by name above — a row that reached here as local would mean the store
            // spelled a switch to the local server some other way, and the honest
            // answer is the daemon default rather than a panic on a fact nobody wrote.
            Ok((crate::slash::ModelChoice::OwnServer, _)) | Err(_) => Some(format!(
                "could not restore {stored}: this build does not know that provider, so the \
                 daemon's own default answers"
            )),
        }
    }

    /// **The operator's pause of this session's plan-check**, or `None` while it runs. See
    /// [`crate::nag_pause`].
    pub fn nag_pause(&self) -> Option<letibot_sessionlog::NagPause> {
        self.cfg.nags_paused
    }

    /// **The pause as the clock arms for it** — the wall-clock end turned into an `Instant` from
    /// now, read fresh at every arming so the clock never holds a pause this session no longer
    /// has. See [`crate::sessions::NagHold`].
    pub fn nag_hold(&self) -> crate::sessions::NagHold {
        use crate::sessions::NagHold;
        use letibot_sessionlog::NagPause;
        match self.cfg.nags_paused {
            None => NagHold::Running,
            Some(NagPause::UntilResumed) => NagHold::Paused { until: None },
            Some(NagPause::Until { ms }) => {
                let left = ms.saturating_sub(letibot_sessionlog::registry::now_ms());
                NagHold::Paused {
                    until: Some(std::time::Instant::now() + std::time::Duration::from_millis(left)),
                }
            }
        }
    }

    /// **`/nag`, on this session** — the report, the pause or the resume, as the verb's reply.
    /// The clock is its owner's to re-arm afterwards (`Sessions`' arm, `serve_child`'s): this
    /// half moves the session's fact and says it.
    pub fn nag_verb(&mut self, verb: crate::nag_pause::NagVerb) -> crate::slash::SlashReply {
        use crate::nag_pause::{self as np, NagVerb};
        let now = letibot_sessionlog::registry::now_ms();
        let lines = match verb {
            NagVerb::Report => vec![np::report(self.cfg.nags_paused, now)],
            NagVerb::Pause(d) => {
                let p = match d {
                    None => letibot_sessionlog::NagPause::UntilResumed,
                    Some(d) => letibot_sessionlog::NagPause::Until {
                        ms: now.saturating_add(d.as_millis() as u64),
                    },
                };
                let mut lines = vec![np::paused_reply(p, now)];
                if let Err(why) = self.pause_nags(p) {
                    lines.push(why);
                }
                lines
            }
            NagVerb::Resume => match self.cfg.nags_paused {
                None => vec!["nags are not paused in this session, so nothing changed".into()],
                Some(_) => {
                    let mut lines = vec![
                        "nags RESUMED for this session: the plan-check is armed again from now"
                            .into(),
                    ];
                    if let Err(why) = self.resume_nags(np::Resumed::ByVerb) {
                        lines.push(why);
                    }
                    lines
                }
            },
        };
        crate::slash::SlashReply { lines, ok: true }
    }

    /// **Pause this session's plan-check** — the config, the row and the hub, together, for
    /// [`Self::stand_down_auto_compact`]'s reason: a reader who finds one without the others has
    /// been lied to. The `nags_paused` row is the loud half; the settings row is what a head that
    /// attaches later reads.
    ///
    /// A row the store refused is SAID — in the hub row and in the `Err` the verb's reply carries
    /// — and the pause still holds in memory: it is real until this daemon stops, and the operator
    /// is told it will not outlast it.
    pub fn pause_nags(&mut self, p: letibot_sessionlog::NagPause) -> Result<(), String> {
        let now = letibot_sessionlog::registry::now_ms();
        self.cfg.nags_paused = Some(p);
        let stored = self.store_nag_pause(Some(p));
        let mut detail = format!(
            "nags PAUSED by the operator {}: this session's plan-check is not armed or sent while it holds",
            crate::nag_pause::until_words(p, now)
        );
        if let Err(why) = &stored {
            detail.push_str(&format!(". {why}"));
        }
        self.hub.publish(SessionEvent::Warning {
            code: "nags_paused".into(),
            detail,
            compaction: None,
        });
        self.publish_settings();
        stored
    }

    /// **End the pause** — by the verb, at its own deadline, or noticed at open after it ran out
    /// while the session was closed. The `nags_resumed` row says which.
    pub fn resume_nags(&mut self, why: crate::nag_pause::Resumed) -> Result<(), String> {
        let Some(was) = self.cfg.nags_paused.take() else {
            return Ok(());
        };
        let stored = self.store_nag_pause(None);
        let mut detail =
            crate::nag_pause::resumed_words(why, was, letibot_sessionlog::registry::now_ms());
        if let Err(why) = &stored {
            detail.push_str(&format!(". {why}"));
        }
        self.hub.publish(SessionEvent::Warning {
            code: "nags_resumed".into(),
            detail,
            compaction: None,
        });
        self.publish_settings();
        stored
    }

    /// The row half of [`Self::pause_nags`] and [`Self::resume_nags`]. No store is no row and no
    /// error: a bare harness has nothing to outlast.
    fn store_nag_pause(&self, p: Option<letibot_sessionlog::NagPause>) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store
            .set_nags_paused_until(&self.cfg.session_id, p.map(|p| p.to_stored()))
            .map_err(|e| {
                format!(
                    "Writing it to the session row FAILED ({e}), so it holds only until this daemon stops"
                )
            })
    }

    /// **A resume comes back with the operator's pause**, from the session's own row — called
    /// at open, for every session that opens, children included.
    ///
    /// **The row is the answer even when it says nothing**: a child's config is a clone of its
    /// parent's, and a parent's pause must not ride into it (see [`crate::nag_pause`]), so a
    /// session with no row, or no store, opens running. A pause whose end passed while nobody
    /// held the session is ended here and said, rather than restored as a pause nothing would
    /// ever lift.
    pub fn restore_nag_pause(&mut self) {
        let stored = self
            .store
            .as_ref()
            .and_then(|s| s.session(&self.cfg.session_id).ok())
            .flatten()
            .and_then(|s| s.nags_paused_until)
            .map(letibot_sessionlog::NagPause::from_stored);
        self.cfg.nags_paused = stored;
        if let Some(p) = stored
            && p.over_at(letibot_sessionlog::registry::now_ms())
        {
            let _ = self.resume_nags(crate::nag_pause::Resumed::WhileClosed);
        }
    }
}

/// How many bytes of a job's output one read returns: 16 KiB, enough that a build
/// log's tail is a single read, with the next offset named when it is not. One
/// constant because the slash reply, the pane's window and any future reader must
/// agree about what "one page" means or they will page each other in circles.
pub const JOB_OUTPUT_WINDOW: usize = 16 * 1024;

/// A window onto a background job's output, with the numbers a pane needs to draw
/// its own header and its own paging.
///
/// [`Harness::job_output`] returns *prose* — a footer sentence with the offsets in
/// it — for a head that will print the lines it was handed, which is what a slash
/// reply is. This is the same read with the offsets **beside** the text, so the
/// jobs pane can say where the window starts, whether anything fell off the front,
/// and where to ask next without parsing a sentence it did not write. The two reads
/// sit on one call — `ProcessHost::output` over the capture ring — and differ only
/// in presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobWindow {
    /// The offset this window actually starts at — the request is clamped to what
    /// survives, not refused.
    pub from: u64,
    /// One past the window's last byte.
    pub to: u64,
    /// Everything the job has written, ever. The denominator.
    pub produced: u64,
    /// Bytes that fell off the front of the ring before this window: what a head
    /// must disclose so a window's start is not read as the job's start.
    pub dropped: u64,
    /// The job's own word — `exited 0`, `running`, `killed by …`.
    pub state: String,
    /// **Whether anything was ever executed for this job** — `true` only for the one
    /// state where the wrapper could not join the cgroup, so a head with an empty
    /// window can say *never ran* rather than *wrote nothing* (A.2, §11.6).
    pub never_ran: bool,
    /// The window, split into lines **here**, so two heads cannot disagree about
    /// where a line ends.
    pub lines: Vec<String>,
    /// Where to ask next when there is more that is still readable, and `None`
    /// when the end is here. Decided here rather than by the head computing `to`,
    /// because only the daemon knows how much of the ring survives.
    pub next: Option<u64>,
}

/// **What the world says about one handle** — the three answers a conditioned row needs, and they
/// are three because a reader acts differently on each.
///
/// **`Unknown` MEETS the condition**, and that is the operator's own requirement read literally:
/// *"a todo conditioned on job end, and then i restart head and harnessd. once server is back it
/// should fire - job is gone"*. A job that is not running has ended, and a handle this daemon has
/// never heard of is what that looks like after a restart. What it does NOT say is what became of
/// the job — which is why the sentence for it sends the reader to look, and why the operator's own
/// split is the one to keep: *"if you want to distinguish - you either follow the job result up
/// manually (which is more robust) if next steps depend on it or just do your things if it was just
/// a timeline"*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobFate {
    /// Still running **here**. The condition is not met.
    Running,
    /// It ended, and this is its own word — `exited 0`, `killed by job_kill`, `signalled 15`.
    Ended { state: String },
    /// **Not a job this session knows**: it ended before this daemon came back, or the handle is
    /// wrong. The condition is met and the result is not here.
    Unknown,
}

/// **One due row, as the sentence the model is handed.** Pure, so the wording is asserted without
/// a session — the same reason [`recorded_job_lines`] is.
///
/// `handle` is passed rather than carried in the fate because a fate is a fact about a job and the
/// handle is how a row names it; the two arrive from different places and only this function has
/// both.
fn fate_line(handle: &str, fate: &JobFate) -> String {
    match fate {
        // Never printed — a running job is not due — and answerable anyway, because a function
        // that panics on the arm nobody reaches is a function whose contract is a rumour.
        JobFate::Running => format!("`{handle}` is still running"),
        JobFate::Ended { state } => format!("`{handle}` {state}"),
        JobFate::Unknown => format!(
            "`{handle}` is not a job this session knows — it ended before this daemon came back, \
             or the handle is wrong. The result is not here: read it up yourself if the next step \
             depends on it."
        ),
    }
}

/// **The decision `Harness::job_fate` makes, as a function of what its two looks returned.**
///
/// Pulled out for the reason [`fate_line`] is pure: this decision is the thing a conditioned row
/// turns on — the operator's own acceptance case is *"a todo conditioned on job end, and then i
/// restart head and harnessd. once server is back it should fire - job is gone"* — and reaching it
/// through a `Harness` needs a backend, a registry, a gate and a store, so nothing asserted it at
/// all. What is left in the method is the two reads; what is here is the ORDER, which is the whole
/// of the honesty: only the process host can say a job runs *now*, and a stored `running` row is a
/// job the daemon was watching when it died, which is a fact about *then*.
fn fate_of(live: Option<letibot_tools::exec::JobState>, record: Option<&JobRecord>) -> JobFate {
    if let Some(state) = live {
        return if state.is_running() {
            JobFate::Running
        } else {
            JobFate::Ended {
                state: state.word(),
            }
        };
    }
    match record {
        Some(row) => JobFate::Ended {
            state: row.state.clone(),
        },
        None => JobFate::Unknown,
    }
}

/// **Which of a board's rows the world now meets** — the filter and the pairing, apart from the
/// harness that owns the rows and the world.
///
/// The board is one union (`TodoBoard::snapshot`), so this walks the whole of it and skips four
/// kinds: **a row with no condition is not this question's subject**, **a running job is not
/// due**, and **a row the idle check may not speak about — one the operator has POSTPONED, or one
/// already `Completed` — is not this question's subject either**. The postponed half is the
/// operator's ask — a postponed item *"persists but without nag"* — and what it means for a
/// conditioned row is exact: the handle is KEPT (nothing here clears it), the row does not fire
/// while it is set aside, and lifting the postponement hands the same question back to this
/// filter, which is what makes the state reversible rather than a quiet way to drop a firing.
///
/// **The fourth skip is this change's, and it is kept rather than reverted.** The skip reads
/// `sessions::the_check_may_ask_about` — the same predicate the idle check arms on and the same
/// one `Harness::nag_notice` reads through — and that predicate's whole answer is *the rows the
/// check may speak about*, which are `Pending` and `InProgress`. Unifying the readers therefore
/// stopped a `Completed` row that still carries a handle from firing, where this filter alone
/// used to let it: a sentence telling the model that finished work is due, which is the nag the
/// rest of the mechanism refuses to send. Reverting that would mean a second predicate and two
/// answers to *what may the check ask about*; it is asserted below instead, so the widened answer
/// is a decision on the record rather than a side effect nobody read.
///
/// Those skips are the whole of the policy, and they are here rather than in the caller
/// so that the decision and the sentence [`fate_line`] writes for it cannot be tested apart from
/// each other — which is what left this piece unasserted until now.
///
/// **`jobs` is the fourth fact the narrowing takes**, beside the board's statuses, the children and
/// the row's own condition: a row that waits on a job this session is running is not one the idle
/// check may speak about, so it must not fire here either. It is handed in rather than looked up so
/// that this stays a function of its arguments — see `Harness::running_jobs` for the one lookup.
fn due_rows(
    rows: &[TodoItem],
    children: &letibot_tools::builtins::todo::ChildSessions,
    jobs: &letibot_tools::builtins::todo::RunningJobs,
    fate: impl Fn(&str) -> JobFate,
) -> Vec<(String, String, JobFate)> {
    // **A firing is a delivery, not a nag**: the grace after an ending (`RunningJobs::digesting`)
    // keeps the CHECK from asking about a row the model has not digested the news for, and this is
    // that news — so it is read without the grace, or a row whose `when` and `wait_on` name one job
    // would hear its condition a turn late.
    let jobs = &jobs.clone().without_grace();
    let mut out = Vec::new();
    for row in rows {
        if !crate::sessions::the_check_may_ask_about(row, children, jobs) {
            continue;
        }
        let Some(letibot_tokencore::store::TodoCondition::Job { handle }) = &row.when else {
            continue;
        };
        let fate = fate(handle);
        if matches!(fate, JobFate::Running) {
            continue;
        }
        out.push((row.content.clone(), handle.clone(), fate));
    }
    out
}

/// **What a recorded job's window says**, apart from the harness that found the row.
///
/// A free function so the words can be asserted without a store, a vocabulary or a session —
/// the same reason [`completion_notices`] is one. What the harness adds around it is a lookup.
fn recorded_job_lines(row: &JobRecord) -> Vec<String> {
    let mut lines = vec![format!(
        "this job is not in this daemon's table: {} after {}, {} bytes out. Its bytes were \
         not kept — the ring died with the process that held them.",
        row.state,
        human_secs(row.elapsed_ms),
        row.produced,
    )];
    if let Some(path) = &row.redirect {
        lines.push(format!(
            "its output went to {path} — that is the file to read."
        ));
    }
    lines
}

impl Harness {
    /// **The window for a handle this daemon never ran**, or the refusal when the store has
    /// never heard of it either.
    ///
    /// The measured complaint: entering a job showed nothing *"whether it went to file or not"*,
    /// and after a restart it showed nothing at all — `no job ... here` about a job whose ending
    /// was written down. This is the durable half answering. It says *not in this daemon's table*
    /// rather than *it ran*, because the row carries a state WORD and this function does not
    /// parse it: a job whose word is `not run (could not join its scope)` must not be described
    /// here as one that ran.
    ///
    /// **What it cannot give is the bytes.** They lived in a ring owned by the process that
    /// wrote them, so they are gone — and the row's `redirect` (R41) is the one thing that can
    /// say where they went instead. A window that invented a page of output would be worse than
    /// an empty one; this one names the loss and, when it can, the file.
    fn recorded_job_window(&self, job: &str) -> Result<JobWindow, String> {
        let Some(row) = self.recorded_job(job) else {
            return Err(format!(
                "no job `{job}` here; `/job` with no argument lists them"
            ));
        };
        let lines = recorded_job_lines(&row);
        Ok(JobWindow {
            from: 0,
            to: 0,
            produced: row.produced,
            // Everything the job wrote is outside this window, which is what `dropped` says.
            dropped: row.produced,
            state: row.state.clone(),
            // Not known here: the row records a state WORD and this function does not parse it.
            never_ran: false,
            lines,
            next: None,
        })
    }

    /// **The stored row for one handle**, or `None` — the one read the output window and the
    /// condition evaluator share, so the two cannot look in different places.
    fn recorded_job(&self, handle: &str) -> Option<JobRecord> {
        let store = self.store.as_ref()?;
        store
            .jobs(&self.cfg.session_id)
            .ok()?
            .into_iter()
            .find(|r| r.handle == handle)
    }

    /// **What the world says about one handle** — see [`JobFate`] for why there are three answers
    /// and why `Unknown` meets a condition.
    ///
    /// **The live table is asked FIRST, and the order is the whole of the honesty.** Only the
    /// process host can say whether a job runs *now*; a stored `running` row is a job the daemon
    /// was watching when it died, which is a fact about *then*. The record is the second look
    /// because it is what survives a restart — and it is why a conditioned row fires at all after
    /// one, which is the case the whole mechanism exists for.
    pub fn job_fate(&self, handle: &str) -> JobFate {
        // **Two looks, and [`fate_of`] is the decision between them** — the live table first,
        // because only the process host can say a job runs *now*, and the record second, because
        // it is what survives a restart.
        let live = self
            .runtime
            .backend
            .processes()
            .and_then(|host| host.job(&letibot_tools::exec::JobId(handle.to_string())))
            .map(|view| view.state);
        fate_of(live, self.recorded_job(handle).as_ref())
    }

    /// **The rows whose condition the world now meets** — the row's text, the handle it waits on,
    /// and the fate — so the caller can both report and CONSUME them in one breath.
    ///
    /// **The fate is carried and not collapsed into a boolean**, because the operator's own split
    /// depends on it: *"you either follow the job result up manually (which is more robust) if next
    /// steps depend on it or just do your things if it was just a timeline"*. A handle that ended
    /// here can be read with `job_output`; one this daemon never heard of cannot, and its sentence
    /// says so rather than leaving the reader to find out by asking.
    ///
    /// **The handle comes back with the row because consuming needs it**, and the condition IS the
    /// handle. A formatter returning only strings would leave the caller re-deriving what it had
    /// just looked up — which is how the two would come to disagree.
    pub fn due_todo_rows(&self) -> Vec<(String, String, JobFate)> {
        // **The rows and both live facts, taken together** — the same reading `nag_notice` takes, so
        // the firing and the nag cannot disagree about whether a row is one the check may speak
        // about: a row waiting on a young child or on a running job is not due by EITHER door.
        let rows = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&rows);
        due_rows(&rows, &children, &jobs, |handle| self.job_fate(handle))
    }

    /// The same read as [`Self::job_output`], as a window rather than a page of
    /// prose — see [`JobWindow`] for why both exist.
    pub fn job_output_window(
        &self,
        job: &str,
        offset: u64,
        limit: usize,
    ) -> Result<JobWindow, String> {
        let Some(host) = self.runtime.backend.processes() else {
            return Err("this session has no process host, so it has no jobs".into());
        };
        let jid = letibot_tools::exec::JobId(job.to_string());
        let Some(view) = host.job(&jid) else {
            // **A job this daemon never ran, but whose ending it may have written down.**
            // The process table is memory: after a restart, a handle the pane listed a moment
            // ago answered `no job ... here` *about a job whose ending is in the store*. The
            // host is still asked FIRST, because only it can say whether a job runs now; the
            // row answers what happened, which is a different question. See [`JobRecord`].
            return self.recorded_job_window(job);
        };
        let slice = host
            .output(&jid, offset, limit)
            .map_err(|e| e.to_string())?;
        // **The decision is [`job_window_for`'s]**, apart from the two reads, for the same
        // reason [`fate_of`] is apart from `job_fate`: this is what a pane turns on, and
        // reaching it through a `Harness` needs a backend, a registry, a gate and a store —
        // so nothing ever held it to a REAL job, which is how the compound-command defect
        // below survived every test this module had.
        Ok(job_window_for(
            &view.command,
            &view.state,
            &slice,
            offset,
            limit,
        ))
    }
}

/// **One live read of a job's output, as the event that answers it** — the half of
/// [`Harness::job_output_window`] that needs only the process host, so it can run on a head's
/// connection thread while the worker runs a turn (`Harness::install_job_output_source`).
///
/// `None` when the host does not hold the job: that answer is the store's record, and the
/// worker reads it. A host that holds the job but cannot read its output is a refusal, said as
/// the same `job_output_refused` warning the queued path publishes.
pub(crate) fn live_job_output(
    host: &dyn letibot_tools::exec::ProcessHost,
    job: &str,
    offset: u64,
    limit: usize,
) -> Option<SessionEvent> {
    let jid = letibot_tools::exec::JobId(job.to_string());
    let view = host.job(&jid)?;
    Some(match host.output(&jid, offset, limit) {
        Ok(slice) => {
            job_window_for(&view.command, &view.state, &slice, offset, limit).into_event(job)
        }
        Err(e) => SessionEvent::Warning {
            code: "job_output_refused".into(),
            detail: e.to_string(),
            compaction: None,
        },
    })
}

impl JobWindow {
    /// The event a head's pane is filled from: the window with the job it belongs to.
    pub fn into_event(self, job: &str) -> SessionEvent {
        SessionEvent::JobOutput {
            job: job.to_string(),
            from: self.from,
            to: self.to,
            produced: self.produced,
            dropped: self.dropped,
            state: self.state,
            never_ran: self.never_ran,
            lines: self.lines,
            next: self.next,
        }
    }
}

/// **The window one read of a job's output answers with** — the decision half of
/// [`Harness::job_output_window`], which does the two reads (the job's row, its capture
/// ring) and hands everything to this.
///
/// **A job whose command redirects its output to a file: the FILE is the window** (R41),
/// and the daemon does the reading — a head has no business reading the box's filesystem,
/// and the path is this side's own record of where the bytes went.
///
/// **The redirect is enough; the capture holding bytes must not un-redirect the job.**
/// The first shape of this answered from the file only when the capture was *completely*
/// empty, which read the command as if a job either redirects or writes but cannot do
/// both. The operator's own workbench said otherwise, measured in their store:
///
/// ```text
/// handle  state      produced  redirect
/// j87     exited 0   2426      /tmp/tpcc-trace2.log   ← the file was NOT read
/// j82     killed     13        /tmp/tpcc-trace.log    ← same shape
/// j70     killed      0        /tmp/tpcc-trace-server.log ← this one worked
/// ```
///
/// j87's command is compound — `sleep 1; head -1 …; echo "=== MIX …"; cd … && ./stroppy
/// run … > /tmp/tpcc-trace2.log 2>&1` — so a preamble of 2426 bytes reached the capture
/// before the work's stage sent its own bytes to the file, and entering the job drew the
/// preamble and never the run. **The trade this takes, stated rather than papered over:
/// when a command names a file, that file is the window whatever the capture holds, and
/// the capture's bytes — the preamble — are not drawn.** They are not lost silently:
/// the fresh read says how many are outside the window and where the window is, the same
/// *counted, not hidden* rule the jobs listing itself follows. The other shapes were
/// rejected on this tree's own rule that `produced`/`from`/`dropped` are one story about
/// one source: splicing the capture in front of the file makes one window out of two
/// coordinate spaces whose offsets cannot mean the same thing, and switching to the file
/// only at settlement hands the pane a `produced` that changes stories the moment the job
/// ends — a livetail of a redirected job wants the file too, because that is where the
/// work has been going all along.
///
/// `offset == 0` is the TAIL — the last `limit` bytes — because that is what an output
/// pane is for and what a fresh read means. Any other offset is a forward window from
/// there, so the ring's own convention holds: `→` asks for the offset `next` named, and
/// a reader can walk to the end of a long log.
fn job_window_for(
    command: &str,
    state: &letibot_tools::exec::JobState,
    slice: &letibot_tools::exec::OutputSlice,
    offset: u64,
    limit: usize,
) -> JobWindow {
    if let Some(path) = letibot_tools::builtins::output_redirect_path(command) {
        return match tail_file(&path, offset, limit) {
            Some(w) => {
                let mut lines = Vec::new();
                // **The preamble, counted rather than hidden.** The capture may hold
                // bytes the redirect never took — a compound command's preamble — and
                // this window is over the FILE, so those bytes are outside it. The
                // fresh read is where the disclosure belongs: `offset == 0` is the
                // read that means *the tail*, and repeating the count above every
                // paged read would be noise, not disclosure.
                if offset == 0 && slice.produced > 0 {
                    lines.push(format!(
                        "{} bytes this job wrote outside the redirect are in its capture, \
                         not in this window — this window is `{path}`",
                        slice.produced
                    ));
                }
                lines.extend(progress_lines(&w.text));
                JobWindow {
                    from: w.from,
                    to: w.to,
                    produced: w.produced,
                    // Everything before the window was not shown: the pane says how much.
                    dropped: w.from,
                    state: state.word(),
                    never_ran: state.never_ran(),
                    lines,
                    next: (w.to < w.produced).then_some(w.to),
                }
            }
            // **The file is not there yet**, or it cannot be read. The sentence that names
            // it is still the honest answer, and it names the command that would read it.
            None => JobWindow {
                from: 0,
                to: 0,
                produced: 0,
                dropped: 0,
                state: state.word(),
                never_ran: state.never_ran(),
                lines: vec![format!(
                    "this job's output goes to `{path}` — the file is not there yet, or \
                     could not be read. `tail -n 50 {path}`"
                )],
                next: None,
            },
        };
    }
    JobWindow {
        from: slice.from,
        to: slice.to,
        produced: slice.produced,
        dropped: slice.dropped,
        state: state.word(),
        never_ran: state.never_ran(),
        lines: if slice.produced == 0 {
            // **Empty, and the redirected case cannot reach here** — a job whose command
            // redirects its output was answered from the FILE above.
            Vec::new()
        } else {
            progress_lines(&slice.text())
        },
        next: next_job_offset(slice),
    }
}

/// **The tail of a file, as the window a pane draws** — the file half of R41's redirected jobs.
///
/// `offset == 0` is the TAIL: the last `limit` bytes, which is what a fresh read of an output
/// pane means. Any other offset is a forward window from there, so the caller can offer the
/// next offset to `→` exactly as it does for a captured job.
///
/// `None` when the file cannot be opened or read — the job has not created it yet, or it is
/// not this process's to read — and the caller then falls back to the sentence that names it.
///
/// **Bytes, not lines, and lossily decoded.** A log is a stream of bytes that happens to be
/// mostly text; a window that refused to draw because one byte in the middle was not UTF-8
/// would be a pane that refuses to show a build log. `progress_lines` does the rest — see it
/// for why the carriage returns are the split's business.
struct FileWindow {
    from: u64,
    to: u64,
    /// The file's whole length, so the pane's `of N` is the file and not the window.
    produced: u64,
    text: String,
}

fn tail_file(path: &str, offset: u64, limit: usize) -> Option<FileWindow> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let produced = f.metadata().ok()?.len();
    let limit = limit as u64;
    let from = if offset == 0 {
        produced.saturating_sub(limit)
    } else {
        offset.min(produced)
    };
    let end = if offset == 0 {
        produced
    } else {
        produced.min(from.saturating_add(limit))
    };
    f.seek(SeekFrom::Start(from)).ok()?;
    // **Read what is actually there**, not the length the metadata promised: a running job
    // appends while this runs, and a file that grew between the two must not fail the read.
    let mut buf = Vec::new();
    f.take(end.saturating_sub(from))
        .read_to_end(&mut buf)
        .ok()?;
    Some(FileWindow {
        from,
        to: from + buf.len() as u64,
        produced,
        text: String::from_utf8_lossy(&buf).into_owned(),
    })
}

/// **The captured bytes, as lines a person can read** — and a progress bar is one line.
///
/// `slice.text()` is a terminal RECORDING, not prose. A tool that draws a progress bar
/// writes `…10%\r…20%\r…100%` with no newline in it, so splitting on `\n` alone puts the
/// whole run of updates on one row: `2%23%47%100%`, which is what the operator read and
/// reported (*"hope to tail its out with ascii codes correctly processed"*).
///
/// The rule is the terminal's own and nothing more: `\r` returns to column 0, and what is
/// written after it OVERWRITES what was there — so the line is the result of writing each
/// segment over the current one, and a shorter final segment leaves the tail of a longer
/// earlier one standing (which is what the screen showed, and not a detail worth losing).
/// No cursor movement beyond the carriage return: `\x1b[A` and friends are stripped
/// downstream by the head like every other escape (§3.1), and emulating them here would be
/// inventing facts about a screen this daemon never had.
///
/// The splitting is the daemon's on purpose — `JobWindow::lines` exists so that two heads
/// cannot disagree about where a line ends — and a carriage return is part of where a line
/// ends, so it belongs here with the split.
fn progress_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|line| {
            if !line.contains('\r') {
                return line.trim_end_matches('\r').to_string();
            }
            // Written over the current row, segment by segment, from column 0.
            let mut row: Vec<char> = Vec::new();
            for (i, seg) in line.split('\r').enumerate() {
                let chars: Vec<char> = seg.chars().collect();
                if i == 0 {
                    row = chars;
                } else {
                    for (col, c) in chars.into_iter().enumerate() {
                        if col < row.len() {
                            row[col] = c;
                        } else {
                            row.push(c);
                        }
                    }
                }
            }
            row.into_iter().collect()
        })
        .collect()
}

/// Where a further read of a job's output would start, or `None` when the window
/// has reached the end of what the ring still holds.
///
/// `produced == dropped + retained` for a ring that only discards from the front,
/// so `to < produced` says exactly "there is more that is still readable" — and the
/// end of what is held is the only place a further read could go. `next` is the
/// offset to ask at, not a count, so a pane can ask at it verbatim.
///
/// A named free function for the same reason [`round_backstop`] is: it is the one
/// piece of arithmetic in the window and a test can hold it to.
fn next_job_offset(slice: &letibot_tools::exec::OutputSlice) -> Option<u64> {
    (slice.to < slice.produced).then_some(slice.to)
}

/// How many times a round is re-attempted when the model endpoint fails.
///
/// **The default for [`Config::http_retries`], and only that** — the RULE is
/// [`letibot_http::retry::Policy`], which is where *what is retryable, and for how long* is
/// written down (three classes, a bounded ladder, and a sentence for each ending). This
/// constant is the number the daemon hands it.
///
/// Five, doubling from a second: 1, 2, 4, 8, 16 — six calls, 31 seconds of waiting. That is
/// the measured 29–31 s llama.cpp cold reload, which is the longest legitimate silence the
/// ladder exists for, and half the 63 s it used to be: the count no longer has to carry the
/// classification, because a refusal is now refused on the first call.
///
/// A caller that already KNOWS the endpoint is not there — a test pointed at a dead port —
/// sets it to 0 and is told so at once. That minute was real: seven tests in `compact.rs`
/// took 63.7 seconds of wall clock for 3.5 seconds of CPU, all of it one of them walking
/// this ladder.
pub const MAX_HTTP_RETRIES: u32 = letibot_http::retry::STANDARD_RETRIES;

/// **Did the MODEL refuse, rather than the wire failing?**
///
/// The discriminator `Harness::fall_back_from` turns on: a `429` or a `5xx` and nothing else.
/// A `408` is the one that looks like it and is not — the server waited too long for US, which
/// is the route rather than a statement about the model — and a transport failure says nothing
/// about it either. The whole of the argument is
/// [`letibot_http::retry::Policy::the_model_refused`], which is where it now lives so that a
/// reader changing what counts as weather sees the retry rule in the same file.
fn model_refused(e: &letibot_turn::HttpError) -> bool {
    letibot_http::retry::Policy::the_model_refused(e)
}

/// **One outage of one model endpoint, and the single row it is worth.**
///
/// # The report this exists for
///
/// MEASURED 2026-10-30, the operator's screen — three rows in three seconds, one per attempt,
/// the same sentence with a different number in it:
///
/// ```text
/// session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:22
/// session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:23
/// session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:25
/// ```
///
/// and their words: *"i dont need walls of this … a coutner is what is need"*.
///
/// # Why the row is written when the outage ENDS, and not at its first failure
///
/// A counter means the row's text has to be current — `attempt 3, last at 10:31:25`, with the
/// LAST attempt's own sentence behind it — and a note on the screen cannot be revised: the head
/// files a warning under `(code, ts, detail)` (`crates/tui/src/app/notes.rs`), so a second
/// publish carrying a bigger count is a SECOND row and not an update of the first. A row that
/// climbs while the outage lasts would therefore need the head, and this daemon does not write
/// there.
///
/// So the count is written where it is already final. The record below is opened by the first
/// failed attempt and **changed by every later one and published never** — a later attempt adds
/// no row, which is the whole of the fix — and it is told once, when the round ends:
///
/// * the round ended on anything but an HTTP failure ⇒ the endpoint answered, and the row says
///   `answered again after 3 attempts / 2m 14s`. A silent recovery is how somebody spends an
///   afternoon believing the endpoint is still down;
/// * the ladder gave up, or the daemon is closing ⇒ `did not answer (attempt 3, last at
///   10:31:25): http 502: …` — the count, when, and the provider's own words, which is what a
///   click opens and what gets pasted into a bug report;
/// * a `[fallback]` moved the session ⇒ that notice is the row and carries the count
///   ([`fallback_notice`]), because two rows about one outage is the wall again.
///
/// **A failure the ladder never retried is not an outage** and writes nothing: with
/// `http_retries = 0` the turn's own `turn_failed` is the whole of the news, and a row saying
/// `attempt 1` beside it would be the same fact twice.
///
/// **Told once, structurally.** [`Harness::settle_outage`] TAKES the record, so no path can tell
/// one outage twice however many ways it ends — the same shape [`Harness::gate_told`] has for the
/// queue, with the key being the live outage itself. A second outage is a second record: the
/// count starts again, because two outages are not one counter.
struct Outage {
    /// The host that did not answer, as [`retry_host`] spelled it. A failure against a DIFFERENT
    /// host is a different outage — the operator's `/models` and a `[fallback]` both move this,
    /// and two endpoints' attempts are not one count.
    host: String,
    /// How many attempts of this outage have failed.
    attempts: u32,
    /// When the first one did, Unix ms — the outage's start, for the duration.
    began_ms: u64,
    /// When the last one did, Unix ms — the `last at 10:31:25`.
    last_ms: u64,
    /// The provider's own sentence for the LAST attempt: the text a bug report wants.
    said: String,
}

impl Outage {
    /// **The row for an outage that ended without an answer** — the count, when the last attempt
    /// was, and the provider's own words.
    ///
    /// `(attempt 3, last at 10:31:25): http 502: …` is one clause and one colon, so the head's
    /// one-line card keeps the count and the time in front of the cut and puts the provider's
    /// sentence behind the click — which is the shape the operator asked for.
    fn did_not_answer(&self) -> String {
        format!(
            "the model endpoint at {host} did not answer (attempt {n}{last}): {said}",
            host = self.host,
            n = self.attempts,
            last = self.last_at(),
            said = self.said,
        )
    }

    /// **The row for an outage that ended because the endpoint came back** — the count, how long
    /// it was out, and the last thing it said before that (which is what a click opens and what
    /// gets pasted into a bug report).
    fn answered(&self, now_ms: u64) -> String {
        format!(
            "the model endpoint at {host} answered again after {n} {word} / {span}. \
             The last thing it said{last}, was: {said}",
            host = self.host,
            n = self.attempts,
            word = if self.attempts == 1 {
                "attempt"
            } else {
                "attempts"
            },
            span = outage_span(now_ms.saturating_sub(self.began_ms)),
            last = self.last_at().replace(", last at", ", at"),
            said = self.said,
        )
    }

    /// `, last at 10:31:25`, or nothing when the clock cannot be read — an empty parenthesis is
    /// worse than the count alone.
    fn last_at(&self) -> String {
        match outage_clock(self.last_ms) {
            Some(at) => format!(", last at {at}"),
            None => String::new(),
        }
    }
}

/// **A wall clock, `10:31:25`** — for the sentence that says when the last attempt failed.
///
/// `None` when the clock cannot be read, and the caller then says the count without the time.
/// The same `localtime_r` shape as `seated_at`, for its reason: a harness is opened from more
/// than one thread, and the `_r` form hands back nothing shared.
fn outage_clock(ms: u64) -> Option<String> {
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    ok.then(|| format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec))
}

/// **How long an outage lasted**, in the words a sentence reads with: `14s`, `2m 14s`, `1h 02m`.
fn outage_span(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// **The retry notice for a session that moves because the model is OUT.**
///
/// Names what refused, what it said, where the session goes and WHEN, and how to come back —
/// five facts, because this is a model change the operator did not type and every one of them is
/// a question they will have. The WHEN is the one that changed on 2026-10-26: the switch takes at
/// the end of this turn, never inside it (see [`Harness::set_provider`]), so the notice must not
/// promise the round continues — it does not. **The WORK continues**, which is a different promise
/// and the daemon's own to keep: [`Harness::continue_after_fallback`] opens the next turn on the
/// model named here.
fn fallback_notice(
    was: &str,
    now: &str,
    e: &letibot_turn::HttpError,
    opens_a_turn: bool,
    outage: Option<&Outage>,
) -> String {
    // **One continuation per fallback, so this branch is real**: a chain whose second name is also
    // out moves the session again and opens nothing, and a sentence promising a turn nobody will
    // run is the kind of lie this tree keeps deleting.
    let on = if opens_a_turn {
        format!(
            "the daemon opens the next turn itself, on {now}, and the work goes on without you."
        )
    } else {
        format!(
            "the daemon opens no second turn of its own — this one was already the continuation a \
             fallback earns — so the next thing you send is what goes to {now}."
        )
    };
    // **The count, when there is one to give** — this notice IS the outage's row when a
    // fallback fires (`Harness::settle_outage` finds the record taken), so the number the
    // retry row would have carried belongs in the sentence that says the ladder is over.
    let after = match outage {
        Some(o) if o.attempts > 1 => format!(" after {} attempts{}", o.attempts, o.last_at()),
        _ => String::new(),
    };
    format!(
        "the model at {was} is out — {e} — and the retry ladder has nothing left to wait \
         for{after}, so this turn ends here and the session moves to {now}, the first name in \
         `[fallback]` that answers. The switch takes when the turn ends and not inside it, \
         because a new model's first request has to follow a user message: {on} Nothing was \
         recorded by the turn that failed. `/models` moves the session back, and the list is in \
         `[fallback] models` in providers.toml."
    )
}

/// **The notice that OPENS the turn a fallback earned** — the model's own copy, as a user item in
/// the harness's voice (`Speaker::Agent`), which is the same shape a plan-check's and a wake's
/// notice take.
///
/// The register is the harness's: `[fallback]` first, like `[todo]`, `[workspace]` and
/// `[reseat]`, so a reader can tell a notice the daemon wrote from the operator's words. What it
/// carries is the three facts the model cannot recover from the transcript alone: **what
/// refused** (this is not the model's own failure), **where it is now** (a different model is
/// answering), and **that nothing was lost** — the failed round committed nothing, so the tool
/// results above it are the work's real state and re-doing them is waste the operator pays for.
fn fallback_continuation_notice(was: &str, now: &str, e: &letibot_turn::HttpError) -> String {
    format!(
        "[fallback] {was} refused ({}); continuing this task on {now}. The turn before this one \
         ended on that refusal and nothing it did was lost — its tool results are above. Pick the \
         work up where it stopped and carry on; do not repeat what is already done.",
        refusal_word(e)
    )
}

/// **The refusal in the few words a notice can afford** — `http 429`, or the kind of failure for
/// one that is not a status at all.
///
/// Deliberately NOT the provider's body: the body goes to the operator in [`fallback_notice`],
/// where a person reading a quota message is worth the tokens. This is the model's copy, and what
/// it needs to know is *the model moved*, not what the old one said.
fn refusal_word(e: &letibot_turn::HttpError) -> String {
    match e {
        letibot_turn::HttpError::Status { code, .. } => format!("http {code}"),
        letibot_turn::HttpError::Io(_) => "the connection failed".to_string(),
        letibot_turn::HttpError::Malformed(_) => "the answer did not parse".to_string(),
    }
}

/// **The host a retry warning names, for the route this turn is on.**
///
/// Split out of the retry loop so the choice is testable on its own, because the wrong
/// answer here is expensive in a way a passing build cannot show: the loop used to name
/// `cfg.endpoint` — the LOCAL endpoint — on a path that also serves cloud turns, so the
/// operator read
///
///   the model server at 127.0.0.1:8080 did not answer: ... Temporary failure in name
///   resolution
///
/// on 2026-10-01 while `llama-server` was answering `/health` with 200 and
/// `letibot --status` reported it `ok`. The failure was a resolver lookup for a provider
/// host; the message sent them to debug the one component that was working.
///
/// So: the local route names the local endpoint, and a cloud route is asked for its own
/// host — the URL it actually posts to, which the operator's key file may have moved.
fn retry_host(provider: Option<&dyn letibot_backend::MessagesBackend>, local: &Endpoint) -> String {
    match provider {
        None => local.authority(),
        Some(p) => p.authority(),
    }
}

/// **What a context-length refusal told us**, in the provider's own numbers.
///
/// `requested` and `limit` are the two numbers the body names — the prompt size it
/// refused and the window it refused against — either of which a body may carry only
/// one of. They are the PROVIDER's counts, not this box's ledger, and a caller that
/// wants to compare them with its own must say which is which rather than silently
/// pairing them (the same rule `Config::ledger_scale` exists for).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContextLengthRefusal {
    requested: Option<u64>,
    limit: Option<u64>,
}

/// **The HTTP status a failure names, when it is a provider refusing what was SENT.**
///
/// 4xx is that class: the request reached a model that read it and said no. 429 is not
/// (the ladder's — it waits, and may move the session), 408 is not (the server waited too
/// long for US, which is the route), and neither is anything 5xx or transport, which is a
/// fact about the endpoint rather than about what was sent. Read by
/// [`Harness::switch_back_notice`].
///
/// **Two shapes**, because a failure now reaches a caller either as it left the engine
/// ([`TurnFailure::Http`]) or as the ladder's account of it ([`HarnessError::Endpoint`]), and
/// a question that answered only the first would silently stop advising a switch back on
/// exactly the failures an operator most wants advice on — a `400` from the model they just
/// moved to.
fn provider_refusal(e: &HarnessError) -> Option<u16> {
    e.http_cause()
        .and_then(letibot_http::retry::Policy::refusal_of_what_was_sent)
}

/// **Is this HTTP failure the context wall, spoken by the thing that owns it?**
///
/// A provider refuses an over-long prompt with a STATUS, not with a wall event —
/// `HttpError::Status` renders as `http {code}: {body}` and lands as a backend error,
/// so every recovery gated on `HarnessError::ContextWall` (the turn tail's
/// `out.is_ok() || wall`) reads false and no door reacts. MEASURED 2026-10-09,
/// session `s-1789919514688401228` on `deepseek/deepseek-flash`: thirty consecutive
/// 400s, the first attributed to `monitor`, the last to `todo check`, none recovered.
///
/// The two measured shapes, verbatim, because a matcher is only as narrow as its
/// fixtures:
///
/// * **llama.cpp**, JSON with a type field — the structured signal, preferred:
///
///   ```text
///   {"error":{"code":400,"message":"request (1504198 tokens) exceeds the available context size (262144 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":1504198,"n_ctx":262144}}
///   ```
///
/// * **an OpenAI-shaped provider**, plain text naming a context length —
///   `error.code` is absent and the numbers live in the sentence:
///
///   ```text
///   This model's maximum context length is 1048576 tokens. However, you requested 1463497 tokens (1463497 in the messages, 0 in the completion). Please reduce the length of the messages or completion. (request_id: 8a448ce7-fabf-4ffa-b350-16e6ddeed6ac)
///   ```
///
/// The rule this implements, and the reason it is ONE function rather than a `contains`
/// at the call site: parse the body as JSON first and read FIELDS
/// (`error.type = "exceed_context_size_error"`, with `n_prompt_tokens`/`n_ctx`; or
/// `error.code = "context_length_exceeded"`, with the numbers parsed out of
/// `error.message`); only a body that is not JSON at all falls back to the text shape,
/// matched by its two anchors (`maximum context length is` and `However, you requested`)
/// and NOT by a bare substring the operator has ruled against (*"prompt-text matching
/// is an overfit"*). Anything else — a 400 about invalid tokens, a 500 about a slot —
/// is a backend error and stays one: if this cannot parse, it does not guess.
fn context_length_refusal(body: &str) -> Option<ContextLengthRefusal> {
    // The structured half: a JSON error object with a field that NAMES the
    // context as the complaint. Both fields are read independently — an
    // OpenAI-shaped body carries `type: "invalid_request_error"` AND
    // `code: "context_length_exceeded"` TOGETHER, so `type` cannot be the
    // only gate or the specific field never gets its say.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let err = v.get("error")?;
        let ty = err.get("type").and_then(|t| t.as_str());
        let code = err.get("code").and_then(|c| c.as_str());
        return match () {
            // llama.cpp: the type field IS the refusal; the numbers are fields.
            _ if ty == Some("exceed_context_size_error") => Some(ContextLengthRefusal {
                requested: err.get("n_prompt_tokens").and_then(|n| n.as_u64()),
                limit: err.get("n_ctx").and_then(|n| n.as_u64()),
            }),
            // OpenAI-shaped: the code field is the refusal; the numbers are in
            // the message, spelled the same way as the plain-text body below.
            _ if code == Some("context_length_exceeded") => {
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
                Some(openai_context_numbers(msg)?)
            }
            // JSON, but about something else — `invalid_request_error` on bad
            // tokens, a quota, a model name. A backend error, not the wall.
            _ => None,
        };
    }
    // The text half: not JSON at all (the measured deepseek body), so the sentence
    // is the only carrier. Both anchors must be present — either alone is a phrase a
    // body about something else could contain.
    openai_context_numbers(body)
}

/// The two numbers out of an OpenAI-shaped refusal sentence, or `None` when the
/// anchors are absent. Kept private to [`context_length_refusal`] so the text shape
/// has one spelling and one place to be narrow.
fn openai_context_numbers(body: &str) -> Option<ContextLengthRefusal> {
    let limit = number_after(body, "maximum context length is")?;
    let requested = number_after(body, "you requested")?;
    Some(ContextLengthRefusal {
        requested: Some(requested),
        limit: Some(limit),
    })
}

/// The digits immediately after `anchor`, skipping spaces — `is 1048576 tokens`,
/// `requested 1463497 tokens`. `None` when the anchor is absent or names no number,
/// because a number this box did not read is a number it must not invent.
fn number_after(body: &str, anchor: &str) -> Option<u64> {
    let at = body.find(anchor)? + anchor.len();
    let digits: String = body[at..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// **Whether a silence notice applies at all, and about which host.**
///
/// `None` for the local route — and that is a policy, not an accident: a local round is
/// silent for 29-31 s reloading a cold GGUF and for minutes on a long prefill, both of
/// which this tree calls *progress*. Split from `SilenceWatch` so the decision can be
/// tested without starting a thread, and from `retry_host` because this one has to
/// answer `None` where that one always names something.
fn silence_host(provider: Option<&dyn letibot_backend::MessagesBackend>) -> Option<String> {
    provider.map(|p| p.authority())
}

/// **How long a CLOUD round may produce nothing before the silence is worth
/// naming.**
///
/// Fifteen seconds, and the number is a judgement rather than a measurement. What IS
/// measured (2026-10-02) is the shape it distinguishes: a degraded DeepSeek answered a
/// streaming request in **12.44 s** to first byte while rejecting a bogus key in
/// 0.33 s and a non-streaming request returned nothing at all in 25 s. So a metered
/// round can sit silent for tens of seconds and then succeed — and until this existed,
/// the operator learned that by asking somebody to run `curl -w`.
///
/// Chosen above the 12.44 s that was measured as *working*, so the notice means "this
/// is slower than a healthy provider", not "this failed".
const SILENT_ROUND_SECS: u64 = 15;

/// **The notice for a cloud round that has produced nothing, when one is
/// warranted.**
///
/// Pure — no clock, no thread — so the decision can be tested on its own. The
/// watchdog below is its only caller.
///
/// The sentence is careful about what is actually known. Nothing here can see the HTTP
/// status, so it does not claim the request was accepted: it says the round has
/// produced nothing, which is the fact, and names the host it is waiting on.
fn silence_notice(
    host: &str,
    silent: std::time::Duration,
    threshold: std::time::Duration,
) -> Option<String> {
    if silent < threshold {
        return None;
    }
    Some(format!(
        "no answer from {host} yet: this round has produced nothing in {}s. It is not a \
         failure — a provider that is slow to start answering looks exactly like this, and \
         so does one that has gone away. The retry ladder is what reports the second, so \
         nothing is being restarted and the request is still open.",
        silent.as_secs(),
    ))
}

/// **Say something WHILE a cloud round is silent**, which is the one thing the
/// operator could not interpret.
///
/// # Why a thread and not a timeout
///
/// The provider blocks its round inside `complete`, so nothing on this thread can
/// notice a silence: the round gets control back only when a delta arrives or the
/// request fails, and both of those are the END of the silence. A watchdog is the only
/// way to speak during the wait.
///
/// # Why it REPORTS rather than aborting
///
/// A first-byte *deadline* that aborted would break the case that was measured to be
/// working: 12.44 s to first byte is a degraded provider that DOES answer, and a
/// deadline short enough to catch it would fail a round that was going to succeed and
/// then retry into the same slow backend. What the operator lacked was not a kill
/// switch but a sentence; the retry ladder already handles a provider that is
/// genuinely gone, and `letibot_http::retry::Policy` is where that policy lives. So this fires
/// once, the round continues, and nothing is cancelled.
///
/// # Scope, and why it is the cloud route only
///
/// A LOCAL round is legitimately silent for 29-31 s reloading a cold GGUF and for
/// minutes on a long prefill — this tree calls both of those *progress*, and the retry
/// constants are written around them. A notice there would fire on healthy work and
/// teach the operator to ignore the code, which is worse than not having it. `None` for
/// the local route is therefore not an omission.
struct SilenceWatch {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SilenceWatch {
    /// Start watching, or return an inert one when there is nothing worth saying.
    ///
    /// `host` is `Some` only for a metered route; see the type's own docs.
    fn start(
        hub: std::sync::Arc<letibot_sessionlog::hub::Hub>,
        host: Option<String>,
        threshold: std::time::Duration,
    ) -> Self {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let Some(host) = host else {
            // Inert: the flag is already set, so `finish` is a no-op and no thread
            // exists to join.
            stop.store(true, Ordering::SeqCst);
            return SilenceWatch { stop, handle: None };
        };
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            // A short slice rather than one sleep of `threshold`, so the round can end
            // promptly without this thread outliving it — the flag is checked between
            // slices, and the thread exits within one slice of the round finishing.
            let slice = std::time::Duration::from_millis(100);
            let started = std::time::Instant::now();
            loop {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                let silent = started.elapsed();
                if let Some(detail) = silence_notice(&host, silent, threshold) {
                    hub.publish(letibot_sessionlog::SessionEvent::Warning {
                        code: "model_slow_first_byte".into(),
                        detail,
                        compaction: None,
                    });
                    // **Once.** A round that is slow for a minute should not fill the
                    // session with the same sentence; the first one carried the news.
                    return;
                }
                std::thread::sleep(slice.min(threshold.saturating_sub(silent)));
            }
        });
        SilenceWatch {
            stop,
            handle: Some(handle),
        }
    }

    /// Stop watching and wait for the thread to notice. Cheap: it is asleep on a 100 ms
    /// slice, so this costs at most that — and nothing at all for the local route, where
    /// no thread was started.
    fn finish(self) {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle {
            let _ = h.join();
        }
    }
}

/// What a job killed with its subagent's VM reads as, on its row.
const VM_LOST: &str = "lost with its VM";

/// **The background jobs a VM child left running, as its parent is told they died** (TODO.md
/// R92.1) — `None` when there were none. Pure, so the wording is asserted without a VM.
pub(crate) fn vm_jobs_lost_line(lost: &[letibot_tools::exec::JobView]) -> Option<String> {
    if lost.is_empty() {
        return None;
    }
    let jobs = lost
        .iter()
        .map(|j| {
            let cmd: String = j
                .command
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
            format!("`{}` (`{cmd}`)", j.label())
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "[subagent jobs] {} background job(s) were still running in the subagent's VM when it \
         was closed after its turn, and died with it: {jobs}. Nothing reported their ending and \
         their output is gone — if the work matters, run it again yourself.",
        lost.len()
    ))
}

/// **What one `/props` probe says about switching to a declared local model** —
/// the decision, or a refusal that names the fact it is missing.
///
/// The free function both doors need, so their answers cannot drift:
/// [`Harness::set_local_model`] for a session switching, and the spawn path for a
/// child seated on a declared local model. Its predecessor answered one question
/// (*are these the weights we tokenize with?*) with a basename comparison and a
/// refusal; the switch this tree owes the operator asks three, and each has its
/// own honest "cannot tell" rather than a default:
///
///   * **which weights answer there** — `/props`'s `model_path`, the vocabulary's
///     one source;
///   * **which template they render** — `/props`'s `chat_template`, the dialect's
///     one source, because what the target SAYS it renders with cannot drift from
///     what it actually renders the way a table of weight names can;
///   * **what to do when they are other weights** — this used to be "refuse, and
///     let the operator start another daemon". Now it is a switch: the target's
///     own vocabulary, loaded from the GGUF it names, and the dialect its
///     template (or the block's hand-asserted `dialect =` key) settles on.
///
/// # The three refusals that are not this function's business
///
/// Loading the vocabulary and resolving the dialect against it happen in the
/// caller, where a failure can leave the caller's state untouched. This function
/// only decides; it probes nothing twice and mutates nothing.
///
/// # Why `same_vocab = true` is read here and not interpreted here
///
/// It is the operator's assertion that the target serves the weights this
/// session's vocabulary already tokenizes for — the same kind of decision on the
/// record `dialect =` is. It short-circuits the weights question because the
/// person who wrote it answered it; it does NOT assert anything about the
/// dialect, which is why the same-weights arm keeps the dialect this session
/// already speaks. A `same_vocab` that also smuggled a dialect change would be a
/// key that guesses, and a guess is the one thing this switch refuses to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalSwitch {
    /// The target serves the weights this session's vocabulary already tokenizes
    /// for — measured by basename, or asserted with `same_vocab = true`. The
    /// engine, dialect and vocabulary all stay; only the address moves.
    ///
    /// A session with no vocabulary of its own (the byte vocabulary a cloud-only
    /// daemon starts on) can never take this arm: there are no weights to be the
    /// same as, so such a switch always seats the target's own.
    SameWeights,
    /// **The target serves other weights**: the switch must load their vocabulary
    /// and seat their dialect, and the conversation must be re-rendered into a
    /// fork before the first turn on them.
    OtherWeights {
        /// The probe everything here was derived from, for the caller's marker
        /// and its report.
        props: letibot_turn::serving::ServedProps,
        /// The dialect the served template (or the block's assertion) settled on.
        dialect: crate::dialect::Dialect,
        /// The GGUF `/props` named, as a path THIS box must be able to read.
        gguf: std::path::PathBuf,
    },
}

/// Decide a switch to a declared local model from ONE `/props` probe.
///
/// Every refusal is a sentence naming the endpoint, what it looked for, and the
/// key that asserts the fact by hand — because "it refused" names nothing and
/// this is the sentence an operator reads at a picker.
///
/// The daemon's own vocabulary is an `Option`, because main made it one: `None`
/// is the byte vocabulary, and there the decision changes shape rather than
/// refusing wholesale — nothing can be `SameWeights`, `same_vocab = true` has
/// no vocabulary to assert against, and the probe alone settles that the switch
/// seats the weights the target itself reports, with their vocabulary and
/// dialect. The refusals that remain name the GGUF the target reported.
pub fn local_switch_decision(
    want: &letibot_turn::http::Endpoint,
    m: &letibot_provider::keys::LocalModel,
    vocab: Option<&std::path::Path>,
) -> Result<LocalSwitch, String> {
    // `same_vocab` is not sampling, so the profile reader files it under
    // `unknown` — which is where a key it does not interpret belongs. Read
    // here, where it means something (see the enum's doc for what it does
    // and does not assert).
    //
    // **But only where there is a vocabulary to be the same as.** A daemon
    // started for a cloud provider has none — the byte vocabulary — and there
    // the assertion cannot be true: the target's weights differ from a
    // vocabulary that does not exist, so NOTHING can be `SameWeights` and the
    // probe below decides alone, whatever the block asserts. The switch that
    // comes out of it is the honest one for that box anyway: the target's own
    // reported weights, with their vocabulary loaded and their dialect seated.
    let asserted_same = vocab.is_some()
        && m.profile
            .unknown
            .iter()
            .any(|u| u == "same_vocab" || u.starts_with("same_vocab "));
    if asserted_same {
        return Ok(LocalSwitch::SameWeights);
    }
    let props = letibot_turn::serving::served_props(want).map_err(|why| match vocab {
        Some(vocab) => format!(
            "{why}, so this cannot check which weights answer there or which \
             template they render. Put `same_vocab = true` in [model.\"{}\"] to \
             assert they are the ones `{}` tokenizes for, or — if they are other \
             weights — a reachable target that answers /props.",
            m.name,
            vocab.display()
        ),
        None => format!(
            "{why}, so this cannot check which weights answer there or which \
             template they render. This daemon has no vocabulary of its own (it was \
             started for a cloud provider, whose turns need none), so there are no \
             weights here a switch could keep and no vocabulary `same_vocab = true` \
             could assert against: the switch to `{}` needs a reachable target that \
             answers /props and names the GGUF it serves, whose vocabulary and \
             dialect are then loaded from that file.",
            m.name
        ),
    })?;
    let Some(theirs) = props.model_path.clone() else {
        return Err(match vocab {
            Some(vocab) => format!(
                "{} answered /props but named no model_path, so there is no GGUF to \
                 load a vocabulary from. Put `same_vocab = true` in [model.\"{}\"] to \
                 assert by hand that it serves the ones `{}` tokenizes for.",
                want.authority(),
                m.name,
                vocab.display()
            ),
            None => format!(
                "{} answered /props but named no model_path, and this daemon has no \
                 vocabulary of its own that `same_vocab = true` could assert against: \
                 no weights here to keep, none reported there to load. The switch to \
                 `{}` was refused and nothing changed.",
                want.authority(),
                m.name
            ),
        });
    };
    if vocab
        .is_some_and(|vocab| vocab_basename(std::path::Path::new(&theirs)) == vocab_basename(vocab))
    {
        // Same weights carry the same template, so the dialect this session
        // speaks is the dialect they render: nothing to re-derive, nothing to
        // re-render, and the switch is the address move it always was.
        // (Unreachable with no vocabulary of our own: a basename cannot match
        // one that does not exist.)
        return Ok(LocalSwitch::SameWeights);
    }
    let dialect = dialect_for_target(&props, m, want).map_err(|why| {
        format!(
            "{}, so the switch to `{}` at {}{} \
             was refused and nothing changed",
            why,
            m.model,
            want.authority(),
            match vocab {
                // With no vocabulary of this daemon's own, the GGUF the target
                // reported is the only weights this switch could ever have
                // seated — so the refusal names it, which is what is true.
                None => format!(
                    " (it reports its weights at `{theirs}`; this daemon has no \
                     vocabulary of its own, so those are the only weights this \
                     switch could ever have seated)"
                ),
                Some(_) => String::new(),
            }
        )
    })?;
    Ok(LocalSwitch::OtherWeights {
        props,
        dialect,
        gguf: std::path::PathBuf::from(theirs),
    })
}

/// **Which dialect does this target render?** — from its own template when it
/// says, from the block's assertion when it cannot, refused when neither speaks.
///
/// The order is the whole decision, and it is not a priority contest:
///
///   * the **served template** matching a dialect this tree drives is the one
///     source that cannot drift from what the server actually renders, so it
///     settles the question outright;
///   * the **asserted `dialect =`** exists for the cases where that source is not
///     available honestly — a server too old to report a template, or one whose
///     template matches nothing this tree drives;
///   * a served template that matches NEITHER dialect with no assertion is a
///     refusal, because the alternative is guessing a dialect for weights it was
///     not written for — valid ids meaning other words, silently;
///   * an assertion that CONTRADICTS a matching template is a refusal too, for
///     the same reason in the other direction: the server's own template is the
///     thing it renders with, and a block that disagrees with it is a mistake
///     somewhere that a person should fix rather than a tie this code breaks.
fn dialect_for_target(
    props: &letibot_turn::serving::ServedProps,
    m: &letibot_provider::keys::LocalModel,
    want: &letibot_turn::http::Endpoint,
) -> Result<crate::dialect::Dialect, String> {
    use crate::dialect::Dialect;
    let all = [Dialect::Glm, Dialect::Qwen];
    // An asserted value that names no dialect is refused rather than dropped:
    // honouring an assertion nobody can read is the regression the profile
    // reader's own test names, one layer down.
    let asserted = m.profile.dialect.as_deref().map(|name| {
        Dialect::parse(name).ok_or_else(|| {
            format!(
                "[model.\"{}\"] asserts `dialect = \"{name}\"`, which is not a dialect \
                 this tree drives; the names are {}",
                m.name,
                all.iter()
                    .map(|d| d.family())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )
        })
    });
    let matched: Vec<Dialect> = props
        .chat_template
        .as_deref()
        .map(|served| {
            all.iter()
                .copied()
                .filter(|d| letibot_turn::serving::template_matches(d.wiring(None).spec(), served))
                .collect()
        })
        .unwrap_or_default();
    match (matched.as_slice(), asserted) {
        // An unreadable assertion is refused before anything else, with its own
        // sentence: a key nobody can honour must not be stepped over, whatever
        // the template said — the operator wrote it, so it is read or refused.
        (_, Some(Err(why))) => Err(why),
        ([], None) if props.chat_template.is_none() => Err(format!(
            "{} did not report a chat_template, so the dialect cannot be derived \
             from what it renders. Put `dialect = \"glm\"` (or `\"qwen\"`) in \
             [model.\"{}\"] to assert one by hand",
            want.authority(),
            m.name
        )),
        ([], None) => Err(format!(
            "{} reports a chat template {} bytes long that matches no dialect \
             this tree drives ({}), so driving it with either renderer would be a \
             guess about weights it was not written for. Put `dialect = \"glm\"` \
             (or `\"qwen\"`) in [model.\"{}\"] to assert one by hand",
            want.authority(),
            props.chat_template.as_deref().map(str::len).unwrap_or(0),
            all.iter()
                .map(|d| d.family())
                .collect::<Vec<_>>()
                .join(" and "),
            m.name
        )),
        ([], Some(Ok(d))) => Ok(d),
        ([only], None) => Ok(*only),
        ([only], Some(Ok(d))) if *only == d => Ok(d),
        ([only], Some(Ok(d))) => Err(format!(
            "{} reports the {} dialect's own chat template, while [model.\"{}\"] \
             asserts `dialect = \"{}\"`. These disagree, and the server's template \
             is the one it renders with — fix the block (or the server) and switch \
             again. Guessing between them is the silent corruption this check exists \
             to refuse.",
            want.authority(),
            only.family(),
            m.name,
            d.family()
        )),
        // Two byte-identical templates would mean two dialects rendering the
        // same bytes; unreachable with the dialects this tree ships, and the
        // honest answer if it ever happens is a person's, not a first-element
        // pick the compiler would silently make of it.
        (many, _) => Err(format!(
            "{} reports a chat template that matches {} dialects this tree drives \
             ({}), which this code cannot choose between",
            want.authority(),
            many.len(),
            many.iter()
                .map(|d| d.family())
                .collect::<Vec<_>>()
                .join(" and ")
        )),
    }
}

/// `http://192.168.1.78:8082` or `192.168.1.78:8082` into an [`Endpoint`].
///
/// A scheme is accepted and discarded rather than refused: the operator writes the
/// url they `curl`, and `[model."x"] url = "http://…"` is the shape that reads
/// naturally beside a key block's `url`. A path is accepted and discarded too —
/// `/v1/chat/completions` is what the curl carries and the client appends its own
/// route, so keeping it would produce `/v1/chat/completions/completion`.
/// **A declared url's `host:port`**, or `None` when it names no usable address.
///
/// `config` needs it to tell which declared model a session is actually on, and it
/// must ask the same parser the switch used: comparing the url text would make
/// `http://192.168.1.78:8082` and `192.168.1.78:8082` two different models, and the
/// operator may have written either.
pub fn local_url_authority(url: &str) -> Option<String> {
    parse_local_url(url).ok().map(|e| e.authority())
}

fn parse_local_url(url: &str) -> Result<Endpoint, String> {
    let rest = url
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let authority = rest.split('/').next().unwrap_or("").trim();
    if authority.is_empty() {
        return Err(format!("`{url}` names no host"));
    }
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| format!("`{url}` names no port; write it as HOST:PORT"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("`{port}` in `{url}` is not a port"))?;
    if host.is_empty() {
        return Err(format!("`{url}` names no host"));
    }
    Ok(Endpoint::new(host, port))
}

/// The GGUF's own file name, which is what two servers holding one model agree on
/// even when the directories differ. A split GGUF's `-00001-of-00006` suffix is kept:
/// two servers that disagree about the shard count are not serving the same file.
/// **Do stored ids mean something else under this vocabulary?** Yes when one side is the
/// byte vocabulary and the other is not, or both are byte tables of different literals.
/// Two GGUFs: not decided here (see the resume that calls this). An empty stored source is
/// a row from before the column was filled, and is taken to be a GGUF — which every such
/// row is.
fn vocab_differs(stored: &str, current: &str) -> bool {
    let bytes = |s: &str| s.starts_with(letibot_tokencore::BYTES_SOURCE);
    match (bytes(stored), bytes(current)) {
        (false, false) => false,
        (true, true) => stored != current,
        _ => true,
    }
}

/// **What is left over when `git worktree add -b <branch>` fails** (TODO R92.17) — a branch an
/// earlier child left behind, or a registration whose directory is gone (locked, or not yet
/// pruned). Said with the commands that clear it; empty when neither is the cause.
fn leftover_hint(workspace: &str, branch: &str, err: &str) -> String {
    let mut hint = String::new();
    let branch_left = git_in(
        workspace,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    if branch_left {
        hint.push_str(&format!(
            " The branch `{branch}` already exists — left by an earlier child. If it is landed \
             or abandoned, `git branch -D {branch}` (after `git worktree prune`) frees the name; \
             otherwise use another `slug`."
        ));
    }
    if err.contains("already registered") || err.contains("locked") || err.contains("missing") {
        hint.push_str(
            " A worktree registration whose directory is gone is in the way: `git worktree \
             prune` clears it (`git worktree unlock <path>` first if it is locked).",
        );
    }
    hint
}

/// **Tell `key` to session `me` once** — once per daemon in `told` (the session's own memory),
/// and once across restarts in the store's `queue_told` (`told:<session>:<key>`, R92.7): a
/// restarted daemon that re-reads a red tip or a parked row does not tell it again, and so a ring
/// about it wakes nobody to nothing. A store that cannot answer is not a reason to stay silent.
fn told_once(
    told: &mut std::collections::HashSet<String>,
    store: &letibot_tokencore::store::Store,
    me: &str,
    key: String,
) -> bool {
    if !told.insert(key.clone()) {
        return false;
    }
    store
        .tell_once(
            &format!("told:{me}:{key}"),
            crate::mergequeue::gate::now_ms(),
        )
        .unwrap_or(true)
}

#[cfg(test)]
mod vocab_differs_tests {
    use super::vocab_differs;

    #[test]
    fn only_a_move_into_or_out_of_bytes_or_between_byte_tables_differs() {
        let (q, g) = ("bytes:aaaaaaaaaaaa", "bytes:bbbbbbbbbbbb");
        assert!(!vocab_differs(q, q));
        assert!(vocab_differs(q, g), "two byte tables mean different ids");
        assert!(vocab_differs(q, "/m/Qwen.gguf"));
        assert!(vocab_differs("/m/Qwen.gguf", q));
        // Two GGUFs are the operator's binding, refused at the switch, not here.
        assert!(!vocab_differs("/m/Qwen.gguf", "/other/Qwen.gguf"));
        // A row from before the column was filled is a GGUF row.
        assert!(!vocab_differs("", "/m/Qwen.gguf"));
        assert!(vocab_differs("", q));
    }
}

/// **Does `pc`'s key fail to resolve** — the one failure a pasted key fixes?
fn key_is_missing(pc: &crate::config::ProviderConfig) -> bool {
    let Ok(preset) = letibot_provider::Preset::parse(&pc.name) else {
        return false;
    };
    matches!(
        letibot_provider::keys::resolve(preset, pc.api_key.as_deref(), None),
        Err(letibot_provider::keys::KeyError::Missing { .. })
    )
}

/// **Would a key saved to providers.toml be the one used?** Not when the command line
/// (`--api-key`) or the environment (`$DEEPSEEK_API_KEY`…) supplies it: both come first in
/// the resolution order, so asking would save a key that is then ignored.
fn key_is_replaceable(pc: &crate::config::ProviderConfig) -> bool {
    if pc.api_key.is_some() {
        return false;
    }
    let Ok(preset) = letibot_provider::Preset::parse(&pc.name) else {
        return false;
    };
    match letibot_provider::keys::resolve(preset, None, None) {
        Ok(c) => !c.from.starts_with('$'),
        Err(_) => true,
    }
}

/// **Why a pasted key cannot be saved**, or `None` when it can. providers.toml is parsed by
/// hand (`keys::parse_file`): a `#` anywhere starts a comment and quotes are stripped, not
/// escaped — so such a key would be saved and read back as something else.
fn unsavable_key(k: &str) -> Option<&'static str> {
    if k.is_empty() {
        Some("it was empty")
    } else if k.contains('#') || k.contains('"') {
        Some("it contains `#` or `\"`, which providers.toml cannot hold")
    } else if k.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some("it contains a space or a control character — likely a paste of more than the key")
    } else {
        None
    }
}

/// How long the key card waits for an answer — longer than sudo's 120 s, because this one
/// may send somebody to a provider's website to make a key.
const KEY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);

/// Why a session on the byte vocabulary cannot be pointed at a local model.
const NO_VOCAB_FOR_LOCAL: &str = "this daemon has no model vocabulary — it was started for a cloud provider, whose \
     turns need none — and a local llama-server reads token ids computed here. Start a \
     daemon with --vocab <the served model's GGUF> to run on a local model.";

/// Why a turn on a daemon with no model chosen is refused — see `Parts::load`.
const NO_MODEL_CHOSEN: &str = "no model is chosen for this session: this box has no local \
     model's vocabulary (GGUF) and no cloud provider. `/models` picks one — a cloud model asks \
     for its key once, and becomes this box's default.";

// **The end-to-end test of the merge queue's enqueue, in its own file** — declared here
// because it is a test of THIS module's private `HarnessTaskRunner`, and because a crate
// whose runner is private can only be tested from inside the crate. Its own header says what
// it closes and what it still cannot reach.
mod clear;
pub use clear::{ClearReport, clear_reply};

// **A transcript whose rows do not rebuild under the prefix it names** — the repair to the fork
// that a store already in that state needs, so a conversation is not refused for ever. See the
// module's header for the shape it is, and why only that shape is repaired.
mod children;
mod open;
mod prefix_repair;
mod wall;
#[cfg(test)]
pub(crate) use children::steer_for_turn;
#[cfg(test)]
pub(crate) use children::{
    AnswerOnce, ChildCommand, PlacedChild, SUBAGENT_TITLE_MAX, TaskSlot,
    a_child_that_ends_stops_its_own, child_command, child_leave_said, children_stopped_notice,
    finish_child_thread, inherited_spawn_model, mint_sub_id_at, name_repos, repos_under,
    stoppable_children, subagent_ask_target, subagent_depth_refusal, subagent_title,
    unsettled_children,
};
pub use children::{
    ChildExit, ChildTurns, SubagentModel, derive_title, message_from_a_session, serve_child_under,
    subagent_model, subagent_model_in,
};
pub(crate) use children::{
    HarnessTaskRunner, TaskSlots, child_mode_source, git_in, steer_for_turn_over,
    stop_children_first, visible_text,
};
pub(crate) use open::{
    STORE_BUSY_PATIENCE, first_sentence, latest_box_stop, load_gguf, provider_notes,
    render_sampling, tool_names, tool_schemas, vocab_basename, wire_turns,
};
pub use open::{
    VerbatimFit, build_provider, engine_for, instructions_of, prefix_fingerprint, scratch_dir,
    stale_prefix_sentence, surroundings_for, verbatim_fit, vm_copy_for, worktree_build_env,
};
#[cfg(test)]
pub(crate) use open::{
    base_role_for_seat, confined_grants, downgraded_ruleset, role_for, role_for_seat,
};
pub use wall::{
    CompactReport, ForkReport, ForkTail, ReseatReport, reseated_for_the_model,
    stale_notice_for_the_model,
};
#[cfg(test)]
pub(crate) use wall::{
    Resident, grown_with_how, roll_resident, schema_change, smaller_tail, wall_stops_round,
};
pub(crate) use wall::{
    UnsealedFork, WallAnchor, compacted_line, reasoning_text, recovered_scale, round_backstop,
    sent_scale, wall_stops_this_round,
};

// **The branch is the deliverable** — the brief's rule checked when a worktree child's turn ends,
// and the line its parent reads when it ends dirty anyway. See the module's header.
pub mod deliverable;

#[cfg(test)]
mod queue_e2e;

#[cfg(test)]
mod tests;

/// **The model half of layer B**, built from the config, or a refusal that names the
/// missing piece.
///
/// One function because two points need it and they must not diverge:
/// `--adjudicator model` puts it in front of the gate alone, and `/mode supervised`
/// puts it in front of a person. A second copy is a second place for the baseline
/// closure to be written slightly differently, and that closure is layer A — the
/// classification the oracle's whole answer is about.
///
/// `asked_by` names whichever of the two asked, so the refusal says which flag or
/// which mode is missing an `--oracle` rather than naming one of them for both.
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
    // **The ceiling, when the operator named one** (R12). The default is the measured one;
    // what this makes possible is acting on the one reading that is a budget — a reply cut
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
        // **The wait, announced before it starts.** Consulting the guard costs
        // seconds on this box, and a turn that pauses with nothing on the screen
        // reads as a hang — the operator went to `htop` to find out whether
        // anything was wrong. Nothing was; the guard was deciding, and nobody
        // said so. `ToolProgress` is the one the head already renders as the
        // latest note on the running call.
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
mod endpoint_retry {
    //! **When the model server is not answering, take the round again.** The
    //! operator, 2026-09-17: *"implement exponential backoff and auto turn
    //! restart for when model http endpoint doesnt answer or answers with error
    //! codes except unauthenticated"*.
    // **The retry rule's own tests are the policy's**, beside it in
    // `crates/http/src/retry.rs`: what is retryable, the ladder's numbers, the billing
    // refusal and the ending sentence are one subject with one home, and a second copy here
    // is a second thing to keep true. What is left in this module is what the DAEMON does
    // with an answer — which host a warning names, what a fallback says, when a silence is
    // worth a row.
    use super::{
        ContextLengthRefusal, Outage, SilenceWatch, context_length_refusal,
        fallback_continuation_notice, fallback_notice, model_refused, outage_span, retry_host,
        silence_host, silence_notice,
    };
    use letibot_turn::{Endpoint, HttpError};

    fn status(code: u16) -> HttpError {
        HttpError::Status {
            code,
            body: String::new(),
        }
    }

    /// The exception is the whole point: a credential the server rejected is
    /// rejected identically every time, so a minute of backoff buys nothing and
    /// buries the real cause under six notices.
    /// **THE SWEEP'S JUDGEMENT, and the defect it exists for, in the operator's words: *"look at the
    /// window number 11 - stuck at the tool and i cant interrupt it"*.**
    ///
    /// MEASURED on that daemon: a `grep` was dispatched, its executor thread and its process both
    /// vanished, and the call sat `running` in the head for eight minutes. `esc esc` was dead
    /// because the interrupt stops GENERATION and generation had already ended — there was no token
    /// to stop and no boundary to reach. Nothing in the daemon looked, because settlement had three
    /// callers and every one of them was the thread that was gone.
    mod abandoned_call_sweep {
        use crate::harness::abandoned_calls;
        use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem, UserPart};

        fn assistant(call_id: &str, name: &str) -> TranscriptItem {
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: call_id.into(),
                    name: name.into(),
                    arguments: "{}".into(),
                }],
                truncated: false,
            }
        }

        fn result_for(call_id: &str) -> TranscriptItem {
            TranscriptItem::ToolResult {
                call_id: call_id.into(),
                name: "grep".into(),
                outcome: ToolOutcome::Ok,
                payload: "ok".into(),
                edit: None,
                origin: None,
                media: None,
            }
        }

        /// The case that cost the operator eight minutes: asked, never answered.
        #[test]
        fn a_call_with_no_result_is_abandoned() {
            let items = vec![assistant("c1", "grep")];
            assert_eq!(
                abandoned_calls(&items),
                vec![("c1".to_string(), "grep".to_string())],
                "a call nothing answered is the whole of what this looks for"
            );
        }

        /// **And the case it must NOT touch.** An answered call is the ordinary one, and a sweep
        /// that flagged it would rewrite every finished round in the transcript.
        #[test]
        fn an_answered_call_is_not() {
            let items = vec![assistant("c1", "grep"), result_for("c1")];
            assert!(
                abandoned_calls(&items).is_empty(),
                "the result answers the call"
            );
        }

        /// **BY ID, not by position and not by name.** Two calls to the same tool are two calls, and
        /// answering the first must not discharge the second — the mistake a name-keyed check makes
        /// silently, because both rows read `grep`.
        #[test]
        fn answering_one_call_does_not_discharge_another_of_the_same_tool() {
            let items = vec![
                assistant("c1", "grep"),
                result_for("c1"),
                assistant("c2", "grep"),
            ];
            assert_eq!(
                abandoned_calls(&items),
                vec![("c2".to_string(), "grep".to_string())],
                "the second grep is still unanswered"
            );
        }

        /// The set difference and not a count: a call asked twice and answered once IS answered.
        #[test]
        fn a_call_asked_twice_and_answered_once_is_answered() {
            let items = vec![
                assistant("c1", "grep"),
                assistant("c1", "grep"),
                result_for("c1"),
            ];
            assert!(abandoned_calls(&items).is_empty());
        }

        /// A transcript with no tool calls at all — the common case — costs nothing and says nothing.
        #[test]
        fn a_conversation_with_no_calls_is_abandoned_by_nothing() {
            let items = vec![
                TranscriptItem::User {
                    parts: vec![UserPart::Text { text: "hi".into() }],
                    speaker: Default::default(),
                },
                TranscriptItem::Assistant {
                    text: "hello".into(),
                    tool_calls: Vec::new(),
                    truncated: false,
                },
            ];
            assert!(abandoned_calls(&items).is_empty());
        }
    }

    /// **"The model is out" is a narrower question than "is this worth waiting for",
    /// and the difference is what decides whether a session MOVES.**
    ///
    /// `fall_back_from` reads this, so an answer that is too wide moves a conversation to
    /// another model over a failure that had nothing to do with the model — a route that is
    /// down, a stream that did not parse, a server that waited too long for us. And one that
    /// is too narrow leaves the operator's quota exhaustion ending a turn, which is the
    /// report this whole change is.
    #[test]
    fn a_model_is_out_only_when_the_model_says_so() {
        // Out: the two the operator's plan produces, and any 5xx.
        assert!(model_refused(&status(429)), "a quota, a rate limit, a plan");
        assert!(model_refused(&status(500)));
        assert!(model_refused(&status(502)));
        assert!(
            model_refused(&status(503)),
            "a server reloading is still out"
        );
        // Not out: the wire, which the ladder waits on and this must not act on.
        assert!(!model_refused(&HttpError::Io(std::io::Error::other(
            "refused"
        ))));
        assert!(!model_refused(&HttpError::Malformed("truncated".into())));
        // And the two that look like timing and are not about the model: 408 is the
        // server waiting too long for US, and a 400 is the request.
        assert!(!model_refused(&status(408)));
        assert!(!model_refused(&status(400)));
        assert!(
            letibot_http::retry::Policy::standard()
                .after(&status(408), 0)
                .is_some(),
            "408 stays on the waiting side, which is exactly why it is not here"
        );
    }

    /// **The fallback sentence, in the words the operator needed to read.**
    ///
    /// It must name what refused, where the session goes, WHEN it goes there, and how to come
    /// back — and it must not promise the round continues, which is what it said until
    /// 2026-10-26: a model's first request has to follow a user message, so the round that met
    /// the refusal ends with it (see `Harness::set_provider`).
    ///
    /// **Two verdicts, because there are two cases**: the usual one opens the continuation turn,
    /// and a chain whose second name is also out opens nothing — the sentence must not promise a
    /// turn the tail will not run.
    #[test]
    fn a_fallback_names_the_way_back_and_says_when_the_switch_takes() {
        let out = || HttpError::Status {
            code: 429,
            body: "Weekly/Monthly Limit Exhausted".into(),
        };
        let moved = fallback_notice(
            "glm-coding/glm-5.3 (metered)",
            "deepseek/deepseek-flash",
            &out(),
            true,
            None,
        );
        assert!(moved.contains("deepseek/deepseek-flash"), "{moved}");
        assert!(
            moved.contains("Weekly/Monthly Limit Exhausted"),
            "what the provider actually said: {moved}"
        );
        assert!(
            moved.contains("[fallback]"),
            "where the choice came from: {moved}"
        );
        assert!(
            moved.contains("when the turn ends"),
            "and when it takes, which is the whole of the change: {moved}"
        );
        assert!(
            moved.contains("the daemon opens the next turn itself"),
            "and that the work goes on without the operator — the 2026-10-12 ruling: {moved}"
        );
        assert!(
            !moved.contains("the round is being taken again"),
            "a round is not re-issued on the new model any more: {moved}"
        );
        assert!(
            moved.contains("/models"),
            "and how a person undoes it: {moved}"
        );

        // **And the case where no turn is opened says so**: the session still moves, and the
        // sentence names what actually happens next at the operator's hand.
        let again = fallback_notice("glm/glm-4.6", "grok/grok-4", &out(), false, None);
        assert!(
            !again.contains("the daemon opens the next turn itself"),
            "a turn nobody will run was promised: {again}"
        );
        assert!(
            again.contains("the daemon opens no second turn of its own")
                && again.contains("the next thing you send is what goes to grok/grok-4"),
            "and the operator is told what does happen: {again}"
        );

        // **And the count, when the ladder has one to give** — this notice IS the outage's row
        // when a fallback fires, so the number the retry row would have carried is here.
        let outage = Outage {
            host: "open.bigmodel.cn".into(),
            attempts: 3,
            began_ms: 0,
            last_ms: 0,
            said: "http 429: Weekly/Monthly Limit Exhausted".into(),
        };
        let counted = fallback_notice("glm/glm-4.6", "grok/grok-4", &out(), true, Some(&outage));
        assert!(
            counted.contains("nothing left to wait for after 3 attempts"),
            "the ladder's own count belongs in the sentence that says it is over: {counted}"
        );
    }

    /// **The one row an outage is worth, in both of its endings** — the count, when the last
    /// attempt was, and the provider's own words, which is what the click opens.
    ///
    /// The clock is the machine's, so the assertions are about the SHAPE the head's one-line card
    /// cuts at (`(attempt 3, last at ` and then the colon) rather than about a literal time —
    /// `crates/harnessd/tests/model_outage.rs` is where the whole sentence is read off a real
    /// daemon, three real attempts and a real head.
    #[test]
    fn an_outage_is_one_row_that_carries_its_count() {
        let outage = Outage {
            host: "api.deepseek.com".into(),
            attempts: 3,
            began_ms: 0,
            last_ms: 0,
            said: "http 502: bad gateway".into(),
        };
        let failed = outage.did_not_answer();
        assert!(
            failed.starts_with("the model endpoint at api.deepseek.com did not answer ")
                && failed.contains("(attempt 3, last at "),
            "the count and when the last attempt was, in the clause a head cuts at: {failed}"
        );
        assert!(
            failed.ends_with("): http 502: bad gateway"),
            "and the provider's own sentence behind it, whole: {failed}"
        );
        let back = outage.answered(134_000);
        assert!(
            back.starts_with("the model endpoint at api.deepseek.com answered again after 3 ")
                && back.contains("attempts / 2m 14s"),
            "a recovery names how many attempts and how long it was out: {back}"
        );
        assert!(
            back.contains("The last thing it said, at ") && back.ends_with("http 502: bad gateway"),
            "and keeps the last thing it said, for the click: {back}"
        );
        // **One attempt is a word, not a count of one** — and a short outage reads in seconds.
        let once = Outage {
            host: "local".into(),
            attempts: 1,
            began_ms: 0,
            last_ms: 0,
            said: "http io: connection refused".into(),
        };
        let said = once.answered(2_400);
        assert!(said.contains("after 1 attempt / 2s"), "{said}");
    }

    /// The duration in the words a sentence reads with — `14s`, `2m 14s`, `1h 02m`.
    #[test]
    fn an_outage_span_is_read_as_a_person_reads_it() {
        assert_eq!(outage_span(0), "0s");
        assert_eq!(outage_span(14_000), "14s");
        assert_eq!(outage_span(134_000), "2m 14s");
        assert_eq!(outage_span(3_720_000), "1h 02m");
    }

    /// **The notice that OPENS the turn a fallback earned**, in the harness's own register —
    /// `[fallback]`, like `[todo]`, `[workspace]` and `[reseat]`, because a head draws the row and
    /// a reader has to be able to tell the daemon's words from the operator's.
    #[test]
    fn the_continuation_notice_names_the_refusal_the_new_model_and_that_nothing_was_lost() {
        let notice = fallback_continuation_notice(
            "glm-coding/glm-5.3",
            "glm/glm-4.6",
            &HttpError::Status {
                code: 429,
                body: "Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-10-12 \
                       15:01:48."
                    .into(),
            },
        );
        assert!(
            notice.starts_with("[fallback] "),
            "the register is the harness's, and it is spelled the way every other notice is: \
             {notice}"
        );
        assert!(
            notice.contains("glm-coding/glm-5.3 refused (http 429)"),
            "what refused, by name and status: {notice}"
        );
        assert!(
            notice.contains("continuing this task on glm/glm-4.6"),
            "where the work goes on: {notice}"
        );
        assert!(
            notice.contains("nothing it did was lost"),
            "and that the failed round's tool results are still the work's state: {notice}"
        );
        assert!(
            !notice.contains("reset at"),
            "the provider's body is the OPERATOR's copy, not the model's — it is in \
             `fallback_notice`: {notice}"
        );
        assert!(
            notice.contains("do not repeat what is already done"),
            "and what to do with it: {notice}"
        );
    }

    /// **The notice fires only past the threshold, and says what it knows.**
    ///
    /// The number in the sentence is the measured silence, not the threshold — the
    /// operator needs to know how long they waited, not what the policy was.
    #[test]
    fn the_silence_notice_needs_the_threshold_passed() {
        let t = std::time::Duration::from_secs(15);
        assert!(
            silence_notice("api.deepseek.com", std::time::Duration::from_secs(14), t).is_none()
        );
        let n = silence_notice("api.deepseek.com", std::time::Duration::from_secs(22), t)
            .expect("past the threshold there is something to say");
        assert!(
            n.contains("api.deepseek.com"),
            "the host is the whole point: {n}"
        );
        assert!(n.contains("22s"), "the measured silence belongs in it: {n}");
        // And it must not claim to know what it cannot: nothing here sees the HTTP
        // status, so `accepted` would be an invention.
        assert!(!n.contains("accepted"), "{n}");
    }

    /// **A round that has produced nothing publishes exactly one notice.**
    ///
    /// MEASURED as the shape this exists for: a degraded DeepSeek took 12.44 s to its
    /// first byte and a NON-streaming request returned nothing in 25 s — and until this,
    /// the operator could not tell either from a provider that was gone without asking
    /// for `curl -w`.
    ///
    /// Once, not once per slice: a round that is slow for a minute must not fill the
    /// session with the same sentence.
    #[test]
    fn a_silent_cloud_round_publishes_one_notice() {
        use letibot_sessionlog::SessionEvent;
        let hub = letibot_sessionlog::hub::Hub::new("silence-cloud");
        let watch = SilenceWatch::start(
            hub.clone(),
            Some("api.deepseek.com".into()),
            std::time::Duration::from_millis(50),
        );
        // Long enough for several 100 ms slices — so a version that published per
        // slice would be caught here rather than in production.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let said: Vec<String> = hub
            .retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::Warning { code, detail, .. } if code == "model_slow_first_byte" => {
                    Some(detail.clone())
                }
                _ => None,
            })
            .collect();
        watch.finish();
        assert_eq!(said.len(), 1, "one notice, not one per slice: {said:?}");
        assert!(said[0].contains("api.deepseek.com"), "{}", said[0]);
    }

    /// **And a local round is never nagged about silence.**
    ///
    /// A cold GGUF reload is 29-31 s and a long prefill is minutes; both are progress,
    /// and the retry constants are written around them. A notice there would fire on
    /// healthy work and teach the operator to ignore the code.
    ///
    /// Two halves, because they are two different mistakes: `silence_host` decides
    /// whether the route qualifies at all (tested here, no thread), and `SilenceWatch`
    /// decides what to do once it does (tested above and below).
    #[test]
    fn a_local_round_is_not_told_its_silence_is_suspicious() {
        use letibot_backend::{BackendCaps, BackendError, Completion, MessagesBackend, StreamFlow};
        use letibot_sessionlog::SessionEvent;

        struct Cloud(&'static str);
        impl MessagesBackend for Cloud {
            fn caps(&self) -> BackendCaps {
                BackendCaps::METERED_API
            }
            fn name(&self) -> &str {
                "deepseek"
            }
            fn model(&self) -> &str {
                "deepseek-flash"
            }
            fn authority(&self) -> String {
                self.0.into()
            }
            fn complete(
                &self,
                _req: &letibot_backend::TurnRequest<'_>,
                _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> StreamFlow,
            ) -> Result<Completion, BackendError> {
                unreachable!("this test is about the route decision, not the round")
            }
        }

        // The decision, first: the local route is not a silence to report.
        assert_eq!(silence_host(None), None, "a local prefill is progress");
        let cloud = Cloud("api.deepseek.com");
        assert_eq!(
            silence_host(Some(&cloud)).as_deref(),
            Some("api.deepseek.com")
        );
        // And it follows the backend's own URL, so a proxy is named as itself.
        assert_eq!(
            silence_host(Some(&Cloud("10.0.0.7:8443"))).as_deref(),
            Some("10.0.0.7:8443")
        );

        // And the mechanism: an inert watch says nothing however long it runs.
        let hub = letibot_sessionlog::hub::Hub::new("silence-local");
        let watch = SilenceWatch::start(
            hub.clone(),
            silence_host(None),
            std::time::Duration::from_millis(50),
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        let said = hub
            .retained()
            .iter()
            .filter(|e| {
                matches!(&e.event,
                    SessionEvent::Warning { code, .. } if code == "model_slow_first_byte")
            })
            .count();
        watch.finish();
        assert_eq!(said, 0, "a local prefill is not a fault");
    }

    /// **A retry notice must name the host that was actually contacted.**
    ///
    /// The loop named `cfg.endpoint` — the LOCAL endpoint — on a path that also serves
    /// cloud turns. MEASURED 2026-10-01: the operator read *"the model server at
    /// 127.0.0.1:8080 did not answer: ... Temporary failure in name resolution"* and went
    /// to a `llama-server` that was answering `/health` with 200, because the failure was
    /// a resolver lookup against a provider host the message never named. The retry
    /// policy was right and the diagnosis was wrong, which is why nothing caught it.
    ///
    /// The override half is the one worth pinning: the answer comes from the backend, so
    /// it follows whatever URL that backend was built with — a proxy or a rented host is
    /// named as itself, not as the preset it replaced.
    /// **The wall as the provider speaks it, recognised.** One test per measured
    /// body, verbatim, because the matcher is only as narrow as its fixtures —
    /// and one per way a body must NOT be read as the wall, because a matcher too
    /// wide sends a session through a compaction it did not need on top of a
    /// failure it did not deserve.
    #[test]
    fn the_two_measured_refusal_bodies_are_recognised_with_their_numbers() {
        // llama.cpp, JSON, the numbers as fields — the structured half.
        let llama = context_length_refusal(
            "{\"error\":{\"code\":400,\"message\":\"request (1504198 tokens) exceeds the available \
             context size (262144 tokens), try increasing it\",\"type\":\"exceed_context_size_error\",\
             \"n_prompt_tokens\":1504198,\"n_ctx\":262144}}",
        )
        .expect("the llama.cpp shape is the wall");
        assert_eq!(
            llama,
            ContextLengthRefusal {
                requested: Some(1_504_198),
                limit: Some(262_144),
            }
        );
        // An OpenAI-shaped provider, plain text, the numbers in the sentence —
        // deepseek's own words on 2026-10-09, request id and all.
        let openai = context_length_refusal(
            "This model's maximum context length is 1048576 tokens. However, you requested \
             1463497 tokens (1463497 in the messages, 0 in the completion). Please reduce the \
             length of the messages or completion. (request_id: \
             8a448ce7-fabf-4ffa-b350-16e6ddeed6ac)",
        )
        .expect("the OpenAI sentence shape is the wall");
        assert_eq!(
            openai,
            ContextLengthRefusal {
                requested: Some(1_463_497),
                limit: Some(1_048_576),
            }
        );
        // And the same refusal as JSON — OpenAI's own envelope, `code` instead
        // of `type`, the message carrying the numbers.
        let openai_json = context_length_refusal(
            "{\"error\":{\"message\":\"This model's maximum context length is 1048576 tokens. \
             However, you requested 1463497 tokens.\",\"type\":\"invalid_request_error\",\
             \"code\":\"context_length_exceeded\"}}",
        )
        .expect("the JSON spelling of the same refusal is the wall");
        assert_eq!(
            openai_json,
            ContextLengthRefusal {
                requested: Some(1_463_497),
                limit: Some(1_048_576),
            }
        );
    }

    #[test]
    fn a_body_that_is_not_a_context_refusal_is_not_the_wall() {
        // A 400 about the TOKENS, not the length — measured in the same log.
        assert!(
            context_length_refusal(
                "{\"error\":{\"code\":400,\"message\":\"Prompt contains invalid tokens\",\
                 \"type\":\"invalid_request_error\"}}"
            )
            .is_none(),
            "an invalid_request_error about tokens is a backend error"
        );
        // llama.cpp's JSON with a DIFFERENT type — the structured field is the
        // signal, not the JSON shape.
        assert!(
            context_length_refusal(
                "{\"error\":{\"code\":400,\"message\":\"no such model\",\"type\":\"model_error\"}}"
            )
            .is_none(),
            "JSON about something else stays a backend error"
        );
        // The OpenAI sentence with one anchor missing: either alone is a phrase
        // a body about something else could contain, so neither alone is the
        // wall.
        assert!(
            context_length_refusal("maximum context length is 4096 tokens, which you may raise")
                .is_none(),
            "a limit without a refusal of THIS prompt is documentation, not the wall"
        );
        assert!(
            context_length_refusal("you requested 1463497 tokens of quota, and the quota is spent")
                .is_none(),
            "a request size without the context anchor is a quota complaint"
        );
        // And the old local failure — `500 Context size has been exceeded`,
        // plain text with neither anchor — stays what it was: unparseable,
        // therefore not guessed at.
        assert!(
            context_length_refusal("Context size has been exceeded").is_none(),
            "a body this cannot parse is not guessed at"
        );
    }

    #[test]
    fn a_retry_names_the_host_that_was_contacted_and_not_the_local_one() {
        use letibot_backend::{BackendCaps, BackendError, Completion, MessagesBackend, StreamFlow};

        struct Cloud(&'static str);
        impl MessagesBackend for Cloud {
            fn caps(&self) -> BackendCaps {
                BackendCaps::METERED_API
            }
            fn name(&self) -> &str {
                "deepseek"
            }
            fn model(&self) -> &str {
                "deepseek-flash"
            }
            fn authority(&self) -> String {
                self.0.into()
            }
            fn complete(
                &self,
                _req: &letibot_backend::TurnRequest<'_>,
                _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> StreamFlow,
            ) -> Result<Completion, BackendError> {
                unreachable!("this test is about the notice, not the round")
            }
        }

        let local = Endpoint::new("127.0.0.1", 8080);
        // The local route keeps naming the local endpoint, which is what it is.
        assert_eq!(retry_host(None, &local), "127.0.0.1:8080");
        // A cloud route names ITS host — not the local one, which was the bug.
        let cloud = Cloud("api.deepseek.com");
        assert_eq!(retry_host(Some(&cloud), &local), "api.deepseek.com");
        assert_ne!(retry_host(Some(&cloud), &local), "127.0.0.1:8080");
        // And it follows the backend's own URL, so a moved endpoint is named as itself.
        let proxy = Cloud("10.0.0.7:8443");
        assert_eq!(retry_host(Some(&proxy), &local), "10.0.0.7:8443");
    }
}

/// One line, at most `n` characters, with the newlines folded — a log row inside a sentence.
fn one_line(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        let cut: String = flat.chars().take(n).collect();
        format!("{cut}…")
    }
}

/// **Calls an assistant row asked for that no row ever answered** — `(call_id, name)`, in the order
/// they were asked.
///
/// The whole of the sweep's judgement, kept apart from the plumbing so it can be tested on its own.
/// It reads ONE transcript, which is why it is right here and not a per-call guess: an assistant row
/// that names a call is the request, a `ToolResult` row with that id is the answer, and a call
/// between the two is one nothing will ever answer.
///
/// **Linear, and that is a requirement rather than tidiness** (R67 item 14). The answered ids were a
/// `Vec` and each asked call was looked up with `iter().any`, which is O(calls × results) over the
/// whole in-memory transcript — and this runs for every held session two seconds after every
/// command or wake, ON THE WORKER. The tree the review measured had ~15k `bash` results, so one
/// sweep of one big session was on the order of 10⁸ string compares with every other session's
/// work queued behind it. A `HashSet` of borrowed ids makes it one pass and one lookup per call,
/// and nothing is cloned that is not returned.
pub fn abandoned_calls(items: &[TranscriptItem]) -> Vec<(String, String)> {
    let mut asked: Vec<(&str, &str)> = Vec::new();
    let mut answered: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for item in items {
        match item {
            TranscriptItem::Assistant { tool_calls, .. } => {
                for c in tool_calls {
                    asked.push((c.id.as_str(), c.name.as_str()));
                }
            }
            TranscriptItem::ToolResult { call_id, .. } => {
                answered.insert(call_id.as_str());
            }
            _ => {}
        }
    }
    // A call asked for TWICE and answered once is answered: the set difference, not a count.
    asked
        .into_iter()
        .filter(|(id, _)| !answered.contains(id))
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect()
}

#[cfg(test)]
mod gatekeeper_reviews {
    //! **The seats a review child and a notes keeper run in, and the mark a head hides them by.**
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

    /// **A session above its repositories is told which ones it can name** — `task_start`'s
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
        // title begins with the mark — so the brief must begin with it, word for word.
        assert!(
            letibot_tools::gatekeeper::review_prompt(&req())
                .starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX),
            "the review brief no longer begins with the head's mark"
        );
    }

    /// **The notes keeper is a seat, and the seat is what confines it.**
    ///
    /// `notes` declares `Access::Write`, so the backend has to be writable — a read-only backend
    /// refuses every repair, and a keeper that cannot repair only complains. What keeps it out of
    /// the code is the SEAT: no `write`, no `edit`, no `bash`, no child to delegate to. Both
    /// halves are asserted here because either one alone reads as the other having been done.
    /// **A reviewer reads the references**: the gatekeeper seat gets `web_search` and
    /// `web_fetch` when the session has them (the operator: reviewers should be able, and
    /// encouraged, to read man pages and docs), and never the room.
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

    /// **The keeper's brief begins with the mark**, on the gatekeeper's rule — so the piece that
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

/// **What a D16 admission says on the transcript** — the one line an operator reads while the
/// work happens.
///
/// The row the gate writes is the record (`/gate`, the corpus); this is the channel beside it,
/// and the only thing that can go wrong in it is a sentence that does not say which command
/// ran or which rule said so. The gate's own half — that the sink is called, once per admit,
/// with the boundary's measured mechanism — is `adjudicate::tests::boundary`'s.
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

    /// One line, naming the command and the rule — and no second `bash` in it.
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
             inside the measured boundary (bubblewrap 0.11.1, writes allowed), D16 · the row \
             and `/gate` carry the full reason"
        );
        assert_eq!(said[0].lines().count(), 1, "a notice is one line");
    }
}
