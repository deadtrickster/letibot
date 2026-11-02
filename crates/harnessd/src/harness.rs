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

    /// **A warning on this session's log** — the operator's own sentence about something that
    /// did not go as asked. One writer for the two doors that can refuse an attachment.
    pub(crate) fn warn(&self, code: &str, detail: String) {
        self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
            code: code.into(),
            detail,
            compaction: None,
        });
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

    /// What a resume rebuilt, or `None` for a session opened fresh.
    pub fn resumed(&self) -> Option<&ResumeReport> {
        self.resumed.as_ref()
    }

    /// The notes `open` made about a session it did not resume — see the field.
    /// Empty for a resumed session, whose notes are in [`Harness::resumed`].
    pub fn open_notes(&self) -> &[String] {
        &self.open_notes
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
mod models;
mod open;
mod prefix_repair;
mod turns;
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
    VerbatimFit, build_provider, engine_for, instructions_of, prefix_fingerprint, scratch_dir,
    stale_prefix_sentence, surroundings_for, verbatim_fit, vm_copy_for, worktree_build_env,
};
#[cfg(test)]
pub(crate) use open::{
    base_role_for_seat, confined_grants, downgraded_ruleset, role_for, role_for_seat,
};
pub use turns::abandoned_calls;
#[cfg(test)]
pub(crate) use turns::reconcile_rows;
pub(crate) use turns::{CapturingSink, MAX_HTTP_RETRIES, Outage, one_line, provider_refusal};
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
