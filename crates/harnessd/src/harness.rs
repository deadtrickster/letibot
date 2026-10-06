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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use letibot_dialect::StablePrefix;
use letibot_sessionlog::event::{TodoEntry, TodoStatus as WireTodoStatus};
use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::{LogSink, SessionEvent, ToolLogSink};
use letibot_tokencore::store::TodoItem;
use letibot_tokencore::store::{JobRecord, SessionRecord, StablePrefixRecord, Store};
use letibot_tokencore::{Vocab, ledger::hex as hex32};
use letibot_tools::authorise::{
    AuthorisationTrail, BreakerState, DenialNotice, DenialSink, Speaker, Utterance,
};
use letibot_tools::builtins::intent::{self as intent_tools, IntentLedger, IntentSink};
use letibot_tools::builtins::todo::{TodoBoard, unfinished_plan};
use letibot_tools::exec::monitor::Monitors;
use letibot_tools::{
    AdjudicatedGate, Adjudicator, Gate, GateCall, HostBackend, NoBoundary, Registry, Role, Tool,
    ToolRuntime, roles,
};
use letibot_transcript::{SystemOrigin, ToolCall, TranscriptItem, UserPart};
use letibot_turn::{
    CompactionOutcome, Endpoint, EventSink, OverrunPlan, Session, SteeringMessage, SteeringSource,
    TailSplit, TurnEngine, TurnEvent, TurnFailure, TurnMetrics, TurnOk, plan_compaction_tail,
    plan_fold, plan_overrun, run_compaction, summarise_first_half, summarise_overrun, tail_because,
    tail_split_of,
};

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
const TRAIL_SCAN: usize = 60;

/// The longest an utterance travels verbatim, matching
/// [`letibot_tools::authorise::Utterance::operator`]'s own rule. A truncated
/// authorisation that reads as complete is worse than a missing one, so a clipped
/// one says it was clipped.
const UTTERANCE_CHARS: usize = 600;

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
    pub(crate) tree_slots:
        Option<std::sync::Arc<std::sync::Mutex<Vec<(String, std::sync::Arc<TaskSlot>)>>>>,
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
}

/// How many ledger rows a resume announces between progress ticks.
///
/// See `Harness::republish`: a tick per row overflowed a head's queue and DEMOTED it, so the
/// restore's own progress bar was killed by the events carrying it.
const FILLING_STRIDE: usize = 64;

impl Parts {
    pub fn load(cfg: &Config) -> Result<Parts, HarnessError> {
        if !cfg.vocab_gguf.is_file() {
            return Err(HarnessError::Setup(format!(
                "no vocabulary GGUF at {}. For a split model, pass the first shard.",
                cfg.vocab_gguf.display()
            )));
        }
        let vocab = Vocab::load(&cfg.vocab_gguf)
            .map_err(|e| HarnessError::Setup(format!("loading the vocabulary: {e}")))?;
        let skills =
            std::sync::Arc::new(letibot_tools::builtins::skill::SkillRegistry::load_default());
        let lsp = std::sync::Arc::new(letibot_tools::builtins::lsp::LspConfig::default());
        let tasks = std::sync::Arc::new(crate::tasks::TaskJournal::new(
            crate::tasks::default_state_path(),
            lsp.clone(),
            skills.clone(),
        ));
        Ok(Parts {
            vocab: std::sync::Arc::new(vocab),
            wiring: std::sync::Arc::new(cfg.dialect.wiring(cfg.effort.as_deref())),
            mode_store: std::sync::Arc::new(
                std::sync::RwLock::new(crate::modes::ModeStore::open()),
            ),
            tasks,
            lsp,
            skills,
            // A root session is its own tree; it has no parent's watcher set to join.
            tree_watch: None,
            // Nor a parent's handle list: a root's runner gets a fresh one (R58).
            tree_slots: None,
            // **Nor a parent's head answers**: a root's own are built as its gate is, and a
            // root with no head-wired adjudicator has none to pass down. See `tree_head`.
            tree_head: None,
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
    ContextWall {
        rounds: usize,
        resident: u64,
        window: u64,
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
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::Setup(s) => write!(f, "{s}"),
            HarnessError::Turn(e) => write!(f, "{e}"),
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
            } => write!(
                f,
                "stopped after {rounds} round(s) at the context wall: {resident} of \
                 {window} tokens resident and the next round would not fit. Everything \
                 this turn produced is committed. A compaction was ATTEMPTED — whether \
                 it succeeded is its own line above, because a summary turn can refuse \
                 (it may not call tools) and saying it worked when it did not is the \
                 failure this whole check exists to avoid. If it compacted, the turn \
                 continues on the summary by itself, a bounded number of times; if the \
                 wall comes back after those, the task does not fit the window and \
                 `/compact` or a fresh session is the way past it."
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
fn reconcile_rows(hub: &Arc<Hub>, sink: &mut CapturingSink, items: &[TranscriptItem]) {
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
}

// ---------------------------------------------------------------------------
// The authorisation trail
// ---------------------------------------------------------------------------

/// One thing that was said in this session, with **who said it** recorded at the
/// moment it was appended.
struct Said {
    speaker: Speaker,
    text: String,
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
    fn say(&self, speaker: Speaker, text: &str, at: Option<Instant>) {
        if text.trim().is_empty() {
            return;
        }
        let mut g = self.lock();
        let turn = g.turn;
        g.said.push(Said {
            speaker,
            text: text.to_string(),
            turn,
            at,
        });
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
                    // Item index as a stand-in turn: the ordering is right and the
                    // distances are monotone, which is what `turns_ago` is read for.
                    turn: i as u32,
                    at: None,
                });
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
            let clipped = said.text.chars().count() > UTTERANCE_CHARS;
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
    /// This session's watcher set, for the settlements it cannot be woken with. `None`
    /// in a test that has no daemon behind it.
    job_watch: Option<Arc<JobWatchers>>,
    /// **This session's children** — the tree's downward edge. An `Interrupt` that reaches a
    /// running turn IS this session being stopped, and a supervisor stops its children before
    /// it goes (see [`HarnessTaskRunner`]). `None` in a test with no session behind it.
    subagents: Option<Arc<dyn letibot_tools::builtins::task::TaskRunner>>,
    /// Messages handed back by a failed turn, drained before the hub's queue so the
    /// next poll finds them first. See [`SteeringSource::give_back`].
    held: VecDeque<SteeringMessage>,
}

impl HubSteering {
    pub fn new(hub: Arc<Hub>) -> Self {
        HubSteering {
            hub,
            trail: None,
            injected: Arc::new(Mutex::new(VecDeque::new())),
            monitors: None,
            monitor_cursor: Arc::new(AtomicUsize::new(0)),
            job_watch: None,
            subagents: None,
            held: VecDeque::new(),
        }
    }
}

/// A monitor firing, as the sentence the model is given.
///
/// It reports **why it fired**, not that it did — T24 requirement 3 — because
/// `Fired::word` distinguishes a firing from an expiry, and a monitor that expired
/// learned nothing about the world.
fn monitor_notice(fired: &[letibot_tools::exec::monitor::Firing]) -> String {
    let mut s = String::from("[monitor] ");
    s.push_str(&format!("{} watch(es) fired:\n", fired.len()));
    for f in fired {
        s.push_str(&format!(
            "  - `{}` ({}), declared by {}: {}\n",
            f.name,
            f.watch,
            f.declared_by,
            f.fired.word(),
        ));
    }
    s.push_str(
        "Only a FIRED watch is an answer about the world; an expiry, a cancellation \
         and an owner ending are three ways a watch stopped existing.",
    );
    s
}

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
        let command = if c.command.is_empty() {
            "command not recorded".to_string()
        } else {
            c.command.clone()
        };
        let whose = if mine {
            String::new()
        } else {
            format!(" (started by `{}`)", c.owner)
        };
        s.push_str(&format!(
            "  - `{}` {} after {}, wrote {} bytes: {}{}\n",
            c.job,
            c.state,
            human_secs(c.elapsed_ms),
            c.produced,
            command,
            whose,
        ));
    }
    if mine {
        s.push_str(
            "This is the completion arriving on its own — you do not need to wait for it, and \
             `job_wait` would only block you for a result you already have. Read what it wrote \
             with `job_output` (job=\"…\"), then carry on with what you were doing.",
        );
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
        if c.detail.is_empty() {
            s.push_str(&format!("  - `{}` {}{}\n", c.job, c.state, whose));
        } else {
            s.push_str(&format!(
                "  - `{}` {}: {}{}\n",
                c.job, c.state, c.detail, whose
            ));
        }
    }
    if mine {
        s.push_str(
            "This is the completion arriving on its own — you do not need to wait for it, and \
             calling `task_result` to block would only hold you for a result you already have. \
             Read what it said with `task_result` (task=\"…\"), then carry on with what you \
             were doing.",
        );
    } else {
        s.push_str(
            "This session is told because the one that started it cannot be: it has stopped and \
             will not run again. Read what it said with `task_result` (task=\"…\") — the handle \
             is in this tree, so the whole answer is still there.",
        );
    }
    s
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
fn completion_notices(done: Vec<JobCompletion>, me: &str) -> Vec<String> {
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
            out.push(completion_notice(&jobs, mine));
        }
        if !tasks.is_empty() {
            out.push(subagent_notice(&tasks, mine));
        }
    }
    out
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
        if let Some(m) = self.held.pop_front() {
            return Some(m);
        }
        // The head first: somebody is waiting on it. Steering-scoped, so a
        // compaction queued behind this turn stays queued for the worker — see
        // [`Hub::try_steering_command`].
        if let Some(cmd) = self.hub.try_steering_command() {
            return match cmd.kind {
                CommandKind::Prompt { text } => {
                    // The operator, typing into a running turn. This is the case
                    // §2's *"yeah restart"* is about, and it is the one utterance
                    // that can authorise the action it is racing. Marked as the
                    // operator's so consecutive prompts coalesce into one held
                    // message and a take-back drops them.
                    if let Some(t) = &self.trail {
                        t.say(Speaker::Operator, &text, Some(Instant::now()));
                    }
                    Some(SteeringMessage::operator(text))
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
                    // **The parent is NAMED in the text, and the trail cannot do it.** `say`
                    // takes a speaker kind, not an identity, so *which* parent spoke survives
                    // nowhere else — and the operator's whole point was addressability: a
                    // correction from a named parent is actionable in a way the same sentence
                    // from nowhere is not.
                    let said = format!("message from your parent session `{from}`: {text}");
                    if let Some(t) = &self.trail {
                        t.say(Speaker::Agent, &said, Some(Instant::now()));
                    }
                    Some(SteeringMessage::normal(said))
                }
                // Nothing else here: the filter above only hands over prompts, interrupts and
                // a parent's message, and anything else stays queued for the worker.
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
        // Then a background job or subagent of THIS session's own that has ended. It is
        // here, above the monitors, because the monitors block below returns early when
        // this session has no monitor registry — and a read-only seat that can only
        // `task` is exactly the session that has completions and no monitors. For a root
        // the set returns nothing here (its door is `Harness::wake`); see [`HubSteering`]
        // for why a child needs this one.
        if let Some(w) = &self.job_watch {
            let done = w.take_mid_turn_completions();
            if !done.is_empty() {
                let text = completion_notices(done, &self.hub.session_id()).join("\n\n");
                if let Some(t) = &self.trail {
                    t.say(Speaker::Agent, &text, Some(Instant::now()));
                }
                return Some(SteeringMessage::normal(text));
            }
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
        let text = monitor_notice(&fired);
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
        for m in msgs {
            self.held.push_back(m);
        }
    }
}

/// One session: the engine, the transcript, the tools, the log and the store.
pub struct Harness<'a> {
    cfg: Config,
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
    /// The server thread answers the head's `Settings` from the registry, so
    /// what is running is what the pane shows.
    session_registry: Arc<letibot_sessionlog::registry::Registry>,
    /// Where the mode came from, for the settings row: the project store or
    /// the daemon's flag/default. Decided at open and not kept by `Config`.
    mode_source: String,
    engine: TurnEngine<'a>,
    session: Session,
    runtime: ToolRuntime,
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

    /// Who said what, so the gate's adjudicator can see what authorised an action.
    /// Fed at every append, because the provenance is only knowable there.
    trail: Arc<TrailMirror>,
    /// The session's todo list, shared with the tool that writes it. The harness
    /// is the persister: [`Harness::flush_todos`] compares the board's version
    /// with the last one it handled and does the store write and the
    /// announcement when they differ.
    todos: Arc<TodoBoard>,
    /// The board version this harness last persisted and announced.
    todos_version: u64,
    /// T21.3's encoder and error signal. Always constructed and always attached —
    /// measuring costs nothing and `Verification::NoEncoder` is a state whose
    /// reason is ours rather than the model's.
    intent: Arc<IntentLedger>,
    /// What the next step boundary will inject: intent findings, monitor firings.
    /// Shared with the session's [`HubSteering`].
    injected: Arc<Mutex<VecDeque<String>>>,
    /// This session's monitors, when the backend can start processes.
    monitors: Option<Arc<Monitors>>,
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
    /// The cloud provider the turns go to, when the session has one. `None` is
    /// the local server through the engine's own `/completion` path.
    provider: Option<Box<dyn letibot_backend::MessagesBackend>>,
    /// **The local server's own context window**, kept from before the first
    /// switch to a provider so `/models local` can put it back.
    ///
    /// `Option<Option<u64>>`: the outer says whether this session has ever left
    /// the local server, the inner is the window itself — which is legitimately
    /// `None` when `/props` reported nothing. Collapsing the two would make
    /// "never switched" and "switched away from a server with no window" the same
    /// state, and only one of them should restore anything.
    local_window: Option<Option<u64>>,
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

/// **What a fork carries verbatim, and why** — R27's ruled tail, as the fork needs it.
///
/// Bundled rather than passed as three arguments because the three are one fact: a tail
/// has contents, it may have started mid-exchange, and it has a reason for being what it
/// is. Splitting them is how one of the three comes to be updated without the others.
pub struct ForkTail<'a> {
    pub items: &'a [TranscriptItem],
    /// Non-zero when the tail could not start at an exchange boundary.
    pub split: Option<TailSplit>,
    /// One of [`letibot_turn::TAIL_BECAUSE`], on the wire as `tail.because`. Empty for a
    /// fork that carries no tail *and* is not a compaction — a re-seat, a re-ingest —
    /// where a reason would be a claim about a decision nobody made.
    pub because: &'static str,
}

impl ForkTail<'_> {
    /// No tail at all, and no reason. What a re-seat and a re-ingest pass.
    pub const NONE: ForkTail<'static> = ForkTail {
        items: &[],
        split: None,
        because: "",
    };
}

/// What a compaction fork actually did — numbers, not the word "compacted".
///
/// The same rule as [`ResumeReport`]: the claim is cheap and the evidence is what
/// makes it checkable. `was_tokens` against `base_tokens` is the reduction an
/// operator can see, and `parent_id` with `forked_at` is where the old
/// conversation went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkReport {
    pub transcript_id: String,
    pub parent_id: String,
    /// The session log's seq at the fork.
    pub forked_at: u64,
    /// The old transcript's token count, measured before anything was dropped.
    pub was_tokens: usize,
    /// The new base: the prefix plus the one summary item.
    pub base_tokens: usize,
    /// **The summary ran out of room before it finished.** Carried so the
    /// operator is told on the same line that says compaction worked -- it did
    /// work, and what it produced is partial, and those are two facts rather
    /// than one.
    pub truncated: bool,
    /// How many of the newest items were carried into the new base verbatim
    /// rather than summarised. Zero is the old behaviour and is still what a
    /// re-seat does, where the point is to change the prefix and not to shorten.
    ///
    /// **Non-zero only for a remote model** (R27, `head-parity-2026-09-21.md`): a
    /// local model is bounded by the KV cache in VRAM, where a tail competes with
    /// the pressure the compaction was called to relieve, and a remote model is
    /// bounded by a bill, where the newest exchanges verbatim are affordable and
    /// buy back exactly what a summary is worst at.
    pub tail_items: usize,
    /// **Items of the newest exchange left out of the verbatim tail**, when the
    /// tail could not start at an exchange boundary — one big file read is larger
    /// than the whole tail budget, so the tail begins inside the exchange that is
    /// still in progress. `None` is the ordinary case: the tail starts where an
    /// exchange does, or there is no tail.
    ///
    /// Disclosed rather than left to be discovered, because a tail that begins
    /// mid-exchange opens with an answer to something that is no longer present,
    /// and the model reading the new base is the reader who has to know that.
    pub tail_dropped: Option<usize>,
    /// **The tail as the wire carries it** — role and text, in order, so a head can draw
    /// the recent past instead of parsing the note's count.
    ///
    /// Role is `operator` or `agent`, the vocabulary `Speaker` already uses. Empty for a
    /// fork with no tail, which is every local compaction and every re-seat.
    pub tail_turns: Vec<letibot_sessionlog::event::CompactionTurn>,
    /// Why the tail is what it is — [`ForkTail::because`], carried through so the report
    /// can put it on the wire without recomputing a decision that has already been made.
    pub tail_because: String,
    /// **The tail as the WIRE describes it**, carrying the reason in the shape both readers
    /// already share.
    ///
    /// `tail_because` above is the same fact as a bare string, and this is the object the
    /// warning carries — `carried` plus `because` plus `dropped` — so the sentence a head
    /// draws and the row this report becomes cannot say different things about one
    /// compaction. [`letibot_sessionlog::event::CompactionTail::why_line`] is the one place
    /// the reason becomes words.
    ///
    /// `None` for a fork that is not a compaction — a re-seat, a re-ingest — where a reason
    /// would be a claim about a decision nobody made.
    pub tail_why: Option<letibot_sessionlog::event::CompactionTail>,
}

/// What a compaction left behind: the fork's numbers and the summary turn's.
///
/// Two reports because two different things happened and a caller discloses both:
/// [`ForkReport`] is what the session's base now is, and `summary_turn` is how the
/// summary was paid for — `cached_tokens` against `reusable` is the measured cache
/// hit the whole cached strategy exists for, and the gap between them is the
/// server's, never ours (the prompts are proven identical over the span).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactReport {
    pub fork: ForkReport,
    pub summary_turn: CompactionOutcome,
    /// **Tools the fork's prompt announces that the old one did not.** A
    /// compaction forks onto the prompt this daemon seats now, so a conversation
    /// started before a tool existed picks it up by compacting — and the operator
    /// is told, because a capability that appears silently is one nobody uses.
    /// Empty on the ordinary compaction, where the two prompts are the same.
    pub gained: Vec<String>,
    /// Tools the old prompt announced and the new one does not.
    pub lost: Vec<String>,
    /// **Did the operator watch the summary being written?**
    ///
    /// The ordinary compaction runs its summary turn through the session's own
    /// sink, so the text streams to every head as it is generated and the fork
    /// that follows needs no announcement. The overrun paths cannot: they
    /// summarise a SCRATCH transcript, and `summarise_one` sends that to a
    /// `NullSink` on purpose — forwarding its progress once made the head draw
    /// the scratch prompt's token count as the session's context. Its comment
    /// ends "progress is the caller's to report, in words, per half", and the
    /// caller reported the start and nothing else.
    ///
    /// Measured 2026-09-20. The operator ran `/compact` on a session 1.5M
    /// tokens over its window; it folded correctly, 1504081 -> 758940 tokens
    /// onto a new transcript with a 2167-token summary as its first item — and
    /// their screen said *"started summarization of the first 1500+ and then
    /// scrolled some s-tasks and that is it, not summary output, nothing"*. The
    /// work was right and the account of it went to the daemon's stderr.
    ///
    /// False means the summary has to be published, because nobody saw it.
    pub summary_was_streamed: bool,
}

/// What a re-seat did: the fork it made, the summary that carried the conversation
/// across, and — the part the operator asked for — which tools the model can now
/// call that it could not before, and which it has lost.
pub struct ReseatReport {
    pub fork: ForkReport,
    pub summary_turn: CompactionOutcome,
    pub gained: Vec<String>,
    pub lost: Vec<String>,
}

/// **A renderer over this box's vocabulary, with no session behind it.**
///
/// For a caller that needs to turn a prompt into tokens and nothing else — the
/// prefix repair does exactly that. Built the same way `open` builds its engine,
/// so what it renders is what a session would.
pub fn engine_for<'a>(parts: &'a Parts, cfg: &Config) -> Result<TurnEngine<'a>, HarnessError> {
    TurnEngine::new(
        &parts.vocab,
        parts.wiring.renderer.as_ref(),
        parts.wiring.parser.as_ref(),
        cfg.endpoint.clone(),
        letibot_backend::BackendCaps::OWN_SERVER,
        cfg.model.clone(),
        cfg.sampling.clone(),
    )
    .map_err(|e| HarnessError::Setup(format!("the dialect does not fit this vocabulary: {e}")))
    .map(|mut e| {
        // **The endpoint's media marker, from `/props`.** `None` for a metered provider or a server
        // without `mtmd`, and then a prompt carrying an image is sent without it — the honest
        // outcome, and the one `delivered` exists to make visible.
        //
        // Set here and at `Harness::open`'s own construction, which is the same seven arguments two
        // lines apart. That duplication is the shape of most of this tree's defects and it is not
        // introduced here; the two sites are named rather than silently left to drift, and
        // collapsing them is a cleanup for its own change because `open` maps the failure to a
        // longer message than this one does.
        e.media_marker = cfg.media_marker.clone();
        e
    })
}

/// The tool names in a rendered schema list.
///
/// Both shapes one is rendered in — OpenAI's `{function:{name}}` and the bare
/// `{name}` — and a schema whose name cannot be read is left out rather than
/// guessed at: this feeds a sentence that tells the operator what changed, and a
/// guess there is worse than a shorter list.
/// A description's first sentence, for a one-line-per-tool listing. The full text
/// is the model's to read; this is the operator's reminder of which tool is which.
fn first_sentence(d: &str) -> String {
    let one = d.split_once(". ").map(|(a, _)| a).unwrap_or(d);
    if one.chars().count() > 76 {
        format!("{}…", one.chars().take(75).collect::<String>())
    } else {
        one.to_string()
    }
}

/// **The tail as the wire carries it**: role and text, in order.
///
/// Only the two item kinds that are *words somebody said*. A `ToolResult` in a tail is a
/// payload — the thing a summary is worst at and the thing this tail exists to keep — but
/// the wire's `CompactionTurn` is `{role, text}` and a tool result is not a turn. Rather
/// than invent a third role for it here, the payload stays in the transcript where it
/// already is and the wire carries the conversation; the count in `tail_items` is what
/// says how much was carried, including anything this mapping does not render.
fn wire_turns(items: &[TranscriptItem]) -> Vec<letibot_sessionlog::event::CompactionTurn> {
    use letibot_sessionlog::event::CompactionTurn;
    items
        .iter()
        .filter_map(|it| match it {
            TranscriptItem::User { parts, .. } => {
                let text = parts
                    .iter()
                    .filter_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty()).then(|| CompactionTurn {
                    role: "operator".into(),
                    text,
                })
            }
            // Reasoning is deliberately NOT a turn: it is the model thinking, not its
            // answer — the same line `compaction::harvest` draws, and the same one
            // `subagent_out_lines` draws. A tail is the literal recent past; the
            // reasoning block is the one part of it nothing downstream reads.
            TranscriptItem::Assistant { text, .. } if !text.is_empty() => Some(CompactionTurn {
                role: "agent".into(),
                text: text.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn tool_names(tools_json: &[String]) -> std::collections::BTreeSet<String> {
    tools_json
        .iter()
        .filter_map(|j| {
            let v: serde_json::Value = serde_json::from_str(j).ok()?;
            let n = v
                .pointer("/function/name")
                .or_else(|| v.get("name"))?
                .as_str()?;
            Some(n.to_string())
        })
        .collect()
}

/// Whether the store already holds this transcript row.
///
/// A `SELECT` through the escape hatch, for the same reason `stored_sessions` in the
/// binary uses one: the append-only guarantees are triggers, so a read here cannot
/// weaken them.
fn transcript_exists(store: &Store, transcript_id: &str) -> bool {
    store
        .connection()
        .query_row(
            "SELECT 1 FROM transcript WHERE id = ?1",
            [transcript_id],
            |r| r.get::<_, i64>(0),
        )
        .is_ok()
}

impl<'a> Harness<'a> {
    /// Open a session: resolve the dialect against the vocabulary, seat the tools,
    /// render the stable prefix, and record all of it.
    ///
    /// Every failure this can raise is one that is otherwise silent at runtime,
    /// which is why they are all raised **here** rather than on the first turn.
    pub fn open(parts: &'a Parts, cfg: Config, hub: Arc<Hub>) -> Result<Self, HarnessError> {
        Self::open_with(parts, cfg, hub, None, None)
    }

    /// As [`Harness::open`], with an adjudicator and an extra tool.
    ///
    /// Uses a fresh, empty session registry: a caller reaching this without one —
    /// every test — gets a harness whose `task` tool cannot spawn a subagent, which
    /// is the honest state for a session nobody registered.
    pub fn open_with(
        parts: &'a Parts,
        cfg: Config,
        hub: Arc<Hub>,
        adjudicator: Option<Box<dyn Adjudicator>>,
        extra_tool: Option<Box<dyn Tool>>,
    ) -> Result<Self, HarnessError> {
        Self::open_with_registry(
            parts,
            cfg,
            hub,
            adjudicator,
            extra_tool,
            letibot_sessionlog::registry::Registry::new(),
        )
    }

    /// [`Harness::open_with`] plus the daemon's session registry, so the `task` tool
    /// can spawn a subagent as a real session (a hub an operator can attach, and a
    /// store row it can resume). The daemon passes its registry; tests and the
    /// single-session binaries pass a fresh one through [`Harness::open`].
    pub fn open_with_registry(
        parts: &'a Parts,
        mut cfg: Config,
        hub: Arc<Hub>,
        adjudicator: Option<Box<dyn Adjudicator>>,
        extra_tool: Option<Box<dyn Tool>>,
        session_registry: Arc<letibot_sessionlog::registry::Registry>,
    ) -> Result<Self, HarnessError> {
        // **The store is opened before anything else, because it may change the
        // configuration.** A session that is already in the store carries its own
        // workspace root, and the tools of a resumed session must be confined to
        // *its* tree, not to whichever directory this daemon happened to start in.
        // Seating a `/home/dead/Projects/rano` conversation with a `/home/dead`
        // backend reads as working — every path resolves — right up to the answer
        // about the wrong file.
        let store = match &cfg.store {
            None => None,
            Some(path) => Some(
                Store::open(path)
                    .map_err(|e| HarnessError::Store(format!("opening {path:?}: {e}")))?,
            ),
        };
        // Opened from the path rather than shared with `store` above: `Store` holds a
        // `rusqlite::Connection`, which is `Send` and not `Sync`, and a corpus sink is
        // shared for the life of the gate. Two connections is the store's declared
        // shape — WAL, a 5 s busy timeout — not a workaround for one.
        //
        // A store that opens for the session and not for the corpus is a real
        // possibility (a read-only mount, a full disk), and it is reported as
        // "not kept" rather than failing the session: a harness that will not start
        // because its telemetry will not start is a worse harness.
        let opened_corpus = cfg
            .store
            .as_ref()
            .and_then(|path| crate::corpus::StoreCorpus::open(path).ok())
            .map(std::sync::Arc::new);
        // As the table holds it, including every earlier run's rows.
        let corpus_counts = opened_corpus.as_ref().map(|c| c.counts().0);
        let corpus_sink: Option<std::sync::Arc<dyn letibot_tools::CorpusSink>> =
            opened_corpus.map(|c| c as std::sync::Arc<dyn letibot_tools::CorpusSink>);
        let stored = match &store {
            None => None,
            Some(s) => s
                .session(&cfg.session_id)
                .map_err(|e| HarnessError::Store(e.to_string()))?,
        };
        let mut notes: Vec<String> = Vec::new();
        if let Some(st) = &stored {
            if !st.workspace_root.is_empty()
                && std::path::Path::new(&st.workspace_root) != cfg.workspace
            {
                notes.push(format!(
                    "the workspace is {} — this session's own, from the store — not {}, \
                     which is where this daemon was started. A resumed session's tools \
                     are confined to the tree the conversation is about.",
                    st.workspace_root,
                    cfg.workspace.display()
                ));
                cfg.workspace = PathBuf::from(&st.workspace_root);
            }
            if let Some(t) = st.title.as_ref().filter(|t| !t.is_empty()) {
                cfg.title = t.clone();
            }
            // **The seat comes from the session too, and it has to land HERE.**
            //
            // The role was already read from the store — 400 lines down, where the
            // registry is resolved — and that was half the job. `cfg.seat` decides
            // which backend gets built (writable, confined, or the read-only
            // default) and whether the session is unconfined, and both of those
            // happen BEFORE that point. So a `leticode` session resumed by a daemon
            // started at the default role seated `write` and `edit` against a
            // READ-ONLY backend: at a mode that requires a writable one the session
            // refused to open at all, and at one that does not it would have opened
            // with tools that fail on their first call.
            //
            // Measured on the operator's own store, 2026-09-15: two resume tests
            // red with "missing: writable backend" for a session whose stored role
            // is `leticode`. The role column exists precisely so a resume does not
            // re-seat a conversation; reading it after the backend is built honours
            // half of that and is worse than not reading it, because the tools and
            // the backend then disagree.
            //
            // Unparseable is an ERROR, same rule as at the registry: falling back to
            // the daemon's role would re-seat the conversation silently.
            if let Some(name) = st.role.as_ref().filter(|r| !r.is_empty()) {
                let seat = Seat::parse(name).map_err(|e| {
                    HarnessError::Setup(format!(
                        "session {} records the role `{name}`, which this build does not know: {e}. Refusing rather than re-seating the conversation with the daemon's own role, which would change its tools without saying so.",
                        cfg.session_id
                    ))
                })?;
                if seat != cfg.seat {
                    notes.push(format!(
                        "the role is `{}` — this session's own, from the store — not \
                         `{}`, which is what this daemon was started with. The tools \
                         AND the backend follow the session's role, or they would \
                         disagree about what it may write.",
                        seat.as_str(),
                        cfg.seat.as_str()
                    ));
                    cfg.seat = seat;
                }
            }
        }

        // **R6: seat an opencode session in ITS directory.** The daemon's start
        // directory is a fact about the daemon; the conversation happened in opencode's
        // `directory`, and a session seated at the wrong root is exactly the
        // `letibot --session` defect the store comment above records. Read **before the
        // backend is built**, because the root is baked into it — the same reason the
        // stored workspace is resolved here and not later. Only for a session that is not
        // yet in the store: a re-run resumes and keeps the root it was imported with.
        if stored.is_none()
            && let Some(oc_id) = cfg.session_id.strip_prefix("oc-").map(str::to_string)
            && let Some(db) = letibot_opencode::default_db_path()
            && let Ok(src) = letibot_opencode::Source::open(&db)
            && let Ok(dir) = src.directory(&oc_id)
            && !dir.is_empty()
        {
            cfg.workspace = PathBuf::from(dir);
        }

        // **The mode is a session property, resolved after the workspace is final.**
        //
        // D13. The workspace was just settled (either the daemon's start directory or
        // this session's own root from the store), so the per-project point can be
        // looked up now — longest ancestor wins inside the store. A project with no
        // row keeps the daemon's `--mode` default; one with a row takes its own, so
        // moving a project never means restarting the daemon.
        let mut mode_source = String::from("daemon flag / default");
        {
            let store = parts.mode_store.read().unwrap();
            // **A subagent keeps its parent's point and never re-reads the row.**
            //
            // `sub_cfg` is built with `..self.base.clone()`, so the child already
            // carries the point the parent resolved — and this lookup then threw it
            // away and took the project's row instead. Two things wrong with that,
            // and the second one is what the operator hit:
            //
            //   * a child could differ from its parent in either direction, which is
            //     the one property a subagent must not have. Authority flows down;
            //     `spec.downgrade` narrows it and nothing widens it.
            //   * a row naming a point the child cannot carry kills the child even
            //     though the parent is running at that very point. Measured
            //     2026-09-20: `/mode allow-all` in leticl refused for the session
            //     (no confinement) and still wrote `allow-all` to the project row,
            //     after which every subagent opened, read the row, failed the
            //     `Confinement` prerequisite and died before its first turn — the
            //     operator saw only *"subagents dont work"*. The parent was fine,
            //     because it had opened before the row was written.
            //
            // So the row is for a session somebody opened in that project. A
            // subagent is not that; it is part of a session whose point is settled.
            if cfg.parent_session_id.is_some() {
                // The point itself is the parent's, read live by the spawn and carried in
                // `cfg.mode`; whether this seat can carry it is decided below, where the
                // seat's own supplies exist. Here is where the row says where it came from.
                mode_source = child_mode_source(None);
            } else if store.is_set(&cfg.workspace) {
                let before = cfg.mode.name;
                cfg.mode = store.for_project(&cfg.workspace);
                mode_source = "project store (modes.tsv)".into();
                if cfg.mode.name != before {
                    notes.push(format!(
                        "mode is `{}` for {} (from the project store), not the daemon default `{before}`",
                        cfg.mode.name, cfg.workspace.display()
                    ));
                }
            }
        }

        // leticode's opencode parity — and a subagent's inheritance of it — is a fact
        // of the *session*, not of the seat. A subagent re-seats to `coder` for its
        // tools, so the seat alone would re-confine it to a project its parent left.
        // Resolve it once here, before the backend, so it is in the `cfg` a subagent
        // clones as its base.
        cfg.unconfined = cfg.unconfined || cfg.seat == Seat::Leticode;

        // **The backend the seat needs, and the constructor is the disclosure.**
        //
        // Four constructors and each is spelled out so `grep -rn
        // 'HostBackend::confined'` answers "which sessions have a boundary" as a
        // fact about the tree rather than a question about it. The read-only one is
        // still the default, because [`Seat::Orchestrator`] is.
        //
        // `HostBackend::executable` is deliberately absent. It buys cgroup lifetime
        // and *no view*: a command started under it runs with this user's rights
        // over this user's whole filesystem, and its own `describe` says `NOT
        // CONFINED` for that reason. Layer 1 exists now, so choosing the
        // unconfined one would be choosing to have no boundary, which is not a
        // choice a daemon should make on an operator's behalf. A box that cannot
        // build a boundary gets an error naming which half failed — no delegated
        // cgroup subtree, or no usable confinement — and not a quiet downgrade.
        //
        // leticode is the exception and the point of it: it is opencode's model, which
        // has no workspace boundary — `read` reaches the whole host and the permission
        // ruleset, not a jail, decides what a write or a command may do. So leticode
        // roots its backend at `/` (unconfined, no namespaces) and leaves the gating to
        // the permission ruleset and the mode. `backend_confined` records which case it
        // was, so the mode's `Confinement` prerequisite is a fact rather than a guess.
        //
        // **A downgrade closes doors here too**, not only at the gate: a subagent
        // denied `exec` gets no process host, and one denied `write` gets a
        // read-only view — the same three facts the disclosure names. Applied
        // before the seat's needs, because a need the downgrade denies is not a
        // need this session has.
        use letibot_tools::schema::Access as Cls;
        let may_exec = !cfg.downgrade.denies(Cls::Exec);
        let may_write = !cfg.downgrade.denies(Cls::Write);
        // **A VM placement is the boundary, so the mode inside is allow-all.** §5:
        // no decision is made because the action is inside. The downgrade still
        // composes on top — it is the caller's explicit ask. Set here, before the
        // gate is built from `cfg.mode`, and disclosed by `placement`.
        let in_vm = cfg.placement == letibot_tools::builtins::task::Placement::Firecode;
        if in_vm {
            cfg.mode = letibot_tools::mode::Mode::ALLOW_ALL;
        }
        let (backend, backend_confined): (HostBackend, bool) = if in_vm {
            // A placeholder the VM arm below replaces; the host tree is not touched
            // by a session placed in a VM, and this read-only view is never used.
            (
                HostBackend::new(&cfg.workspace).map_err(|e| {
                    HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace))
                })?,
                true,
            )
        } else if cfg.unconfined {
            let b = if cfg.allow_bash && may_exec {
                HostBackend::executable("/")
            } else if may_write {
                HostBackend::writable("/")
            } else {
                HostBackend::new("/")
            }
            .map_err(|e| HarnessError::Setup(format!("whole-host backend: {e}")))?
            // Reaching the whole host is not the same as starting at its root: a
            // relative path, and a command's `cwd`, start in the workspace.
            .with_cwd(&cfg.workspace)
            .map_err(|e| {
                HarnessError::Setup(format!("workspace {:?} as cwd: {e}", cfg.workspace))
            })?;
            (b, false)
        } else if cfg.seat.needs_exec_backend() && may_exec {
            (
                HostBackend::confined_granting(
                    &cfg.workspace,
                    cfg.grants_ro
                        .iter()
                        .map(|p| letibot_tools::exec::confine::Grant::ReadOnly {
                            path: p.clone(),
                            why: "granted on the command line with --grant-ro".into(),
                        })
                        .collect(),
                )
                .map_err(|e| {
                    HarnessError::Setup(format!(
                        "the `{}` role needs a confined execution backend and one could not be \
                         built over {:?}: {e}.\n\nThis is a refusal, not a degradation: the \
                         alternative is `HostBackend::executable`, which gives a process the \
                         cgroup that bounds its lifetime and NO boundary on what it can read \
                         — the operator's whole filesystem, with a banner that would have to \
                         say so. Two things it needs and the error above says which is \
                         missing: a delegated cgroup v2 subtree, and a usable unprivileged \
                         namespace boundary (bwrap). A third thing the message above may \
                         name instead: the project root itself. Both `runner` and `coder` \
                         root their view at the workspace, so a workspace that is a file, \
                         a dangling symlink or absent fails here and no boundary is the \
                         wrong thing to blame. Seat `--role orchestrator` for a session \
                         with no exec path at all.",
                        cfg.seat.as_str(),
                        cfg.workspace
                    ))
                })?,
                true,
            )
        } else if cfg.seat.needs_writable_backend() && may_write {
            (
                HostBackend::writable(&cfg.workspace).map_err(|e| {
                    HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace))
                })?,
                false,
            )
        } else {
            (
                HostBackend::new(&cfg.workspace).map_err(|e| {
                    HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace))
                })?,
                false,
            )
        };
        // The home the backend expands `~` against is the session's, read once from
        // the surroundings the gate will also carry — so a `~` and the gate's own
        // `region_of` agree about where the operator lives.
        let surroundings = surroundings_for(&cfg);
        let backend = backend.with_home(
            surroundings
                .home
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_default(),
        );
        // A head's Ctrl+B reaches the `bash` wait loop through the hub's promote
        // channel, so the request is honoured while the worker is blocked inside it.
        let backend = backend.with_promote_channel(hub.promote_channel());
        // The session's scratch directory, where tools put working artifacts that
        // are too big for the transcript. Per-session (one per daemon process) and
        // in `/tmp`, so a fetched page never touches the operator's tree. A backend
        // that cannot reach it — a confined session rooted at the workspace — simply
        // has no scratch, and a tool that wants one gets a refusal rather than a guess.
        let scratch = scratch_dir();
        // **0700, not the process umask's default.** `create_dir_all` cannot take a
        // mode, and the default 0755 leaves the scratch world-readable wherever the
        // parent already existed — which is every later start once the runtime dir is
        // there. The mode applies to every component this creates; the ones that
        // already exist were 0700 when they were made, or are the runtime dir itself.
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut b = std::fs::DirBuilder::new();
            b.mode(0o700).recursive(true);
            if let Err(e) = b.create(&scratch) {
                // stderr rather than a log framework: this crate has none, and a
                // scratch that cannot be made is a start-up fact the operator sees
                // beside the daemon's own banner or nowhere.
                eprintln!("letibot: scratch dir {}: {e}", scratch.display());
            }
        }
        let backend = backend.with_scratch_dir(&scratch);
        // One boxed backend from here on, whichever substrate: the host one built
        // above, or a firecode VM booted on a copy of the workspace.
        let (backend, backend_described, backend_writable, monitors): (
            Box<dyn letibot_tools::backend::ExecBackend>,
            String,
            bool,
            Option<Arc<Monitors>>,
        ) = if in_vm {
            let mut spec =
                letibot_tools::firecode::FirecodeSpec::new(&cfg.workspace, &cfg.session_id);
            spec.writable = may_write;
            spec.exec = may_exec;
            spec.up_args = cfg.vm_args.clone();
            let fc = letibot_tools::firecode::FirecodeBackend::up(&spec).map_err(|e| {
                HarnessError::Setup(format!("placing this session in a firecode VM: {e}"))
            })?;
            let described = fc.describe();
            let writable = fc.is_writable();
            let monitors = fc.host_processes().and_then(|h| h.monitors().cloned());
            (Box::new(fc), described, writable, monitors)
        } else {
            let described = backend.describe();
            let writable = backend.is_writable();
            // **What this daemon manages, declared before any tool runs**: the
            // model server it is talking to. `pkill` lists it as PROTECTED and
            // refuses it by name; a `process` monitor never finds it. Outlives
            // the turn — it is nobody's job.
            if let Some(h) = backend.host_processes() {
                h.protect_listener(
                    cfg.endpoint.port,
                    format!(
                        "the model server this session talks to, on {}",
                        cfg.endpoint.authority()
                    ),
                    true,
                );
                // **What every command carries.** The way back to this daemon
                // (`letibot-askpass` uses it to put a sudo password prompt in
                // front of the head), `HOME`, and — when the helper is beside
                // this binary — a `sudo` shim ahead of `PATH` that asks the head
                // instead of a terminal. See `crate::sudo`.
                let mut env = vec![
                    (
                        "LETIBOT_SOCKET".to_string(),
                        cfg.socket.display().to_string(),
                    ),
                    ("LETIBOT_SESSION".to_string(), cfg.session_id.clone()),
                ];
                if let Ok(home) = std::env::var("HOME") {
                    env.push(("HOME".to_string(), home));
                }
                match crate::sudo::install() {
                    Ok(plumbing) => {
                        env.push((
                            "SUDO_ASKPASS".to_string(),
                            plumbing.askpass.display().to_string(),
                        ));
                        h.set_path_prefix(Some(plumbing.shims.display().to_string()));
                    }
                    Err(why) => {
                        eprintln!("letibot: sudo will have no way to ask for a password: {why}")
                    }
                }
                h.set_standing_env(env);
            }
            let monitors = backend.host_processes().and_then(|h| h.monitors().cloned());
            (Box::new(backend), described, writable, monitors)
        };

        // Retrieval is inert and stays inert: `ask_code` and `ask_corpus` abstain,
        // and nothing is behind them. T16.6 checked rather than recalled — no MCP
        // server is running on this box or on lubuntu3. A stub that answered would
        // be the exact failure §8.2 exists to prevent.
        let retrieval: Arc<dyn letibot_tools::builtins::retrieval::Retrieval> =
            Arc::new(letibot_tools::builtins::retrieval::Unavailable);
        // The web, the forge and MCP have nothing behind them either. They are now
        // **registered** rather than absent, which is a different fact and the one
        // §8.4 asks for: the registry is a superset and the role decides what a
        // session's prompt carries. A seat that names `web_search` gets a tool that
        // refuses with `not_run` naming what is missing; a seat that does not name
        // it never sees it. Neither is a stub that answers.
        // **Nothing is attached unless the operator attached it.** A search
        // provider is egress and a credential; the default stays the tool that
        // refuses and says what would attach one.
        let mut external = letibot_tools::ExternalBackends::unattached();
        if let Some(name) = cfg.web_search.as_deref() {
            match name {
                "brave" => match letibot_websearch::Brave::attach(None) {
                    Ok(b) => external.search = std::sync::Arc::new(b),
                    // Refuse at open, not at the first search: a 401 three seconds
                    // into a turn names neither the key nor the file it is missing
                    // from. `web_search` stays the refusing tool and the banner
                    // says why.
                    Err(why) => {
                        return Err(HarnessError::Setup(format!("--web-search brave: {why}")));
                    }
                },
                other => {
                    return Err(HarnessError::Setup(format!(
                        "--web-search {other}: this build has `brave`"
                    )));
                }
            }
        }
        if cfg.web_fetch {
            match letibot_webfetch::CurlFetcher::attach() {
                Ok(f) => external.fetch = std::sync::Arc::new(f),
                // Refuse at open, not at the first fetch: a missing curl
                // three seconds into a turn names neither the binary nor the
                // flag that would have attached it. `web_fetch` stays the
                // refusing tool and the banner says why.
                Err(why) => return Err(HarnessError::Setup(format!("--web-fetch: {why}"))),
            }
        }
        let external = external;
        // T21.3's two halves, both constructed for every session. The ledger is the
        // error signal and the sink is the encoder; attaching them costs nothing and
        // not attaching them makes every completion `Verification::NoEncoder`, which
        // is a state whose reason is ours.
        let intent = Arc::new(IntentLedger::new());
        let intent_wiring = intent_tools::Wiring {
            ledger: intent.clone(),
            ..intent_tools::Wiring::standalone()
        };
        // **`--role planner` IS plan mode**, so the state says so.
        //
        // Without this, `PlanMode::active` is false for the seat whose whole name is
        // plan mode, `exit_plan_mode` answers *"this session is not in plan mode"* on
        // every call, and one of the role's eight seats is a tool that can only
        // refuse. Fail-closed and honest, and still a seat spent on a tool the model
        // will try.
        //
        // The turn and call ids are this seat rather than a decision: nothing
        // *entered* — no `enter_plan_mode` call exists to point at, because no role
        // seats it — and `PlanState::entered_by` is read by a refusal that has to
        // name what is in force. Naming the role is true; inventing a call id would
        // point a refusal at a decision nobody took.
        if cfg.seat == Seat::Planner {
            intent_wiring
                .plan
                .enter("open", &format!("--role {}", cfg.seat.as_str()));
        }

        let mut registry: Registry = letibot_tools::read_only_tools(retrieval.clone())
            .map_err(|e| HarnessError::Setup(format!("registering the M1 tool set: {e}")))?;
        // The session's todo list, restored before the tool that writes it is
        // seated: a resume comes back with the plan the model was working from,
        // not with an empty pane it must fill from memory. Storeless runs still
        // get the tool — the list lives for the session either way — they just
        // have nowhere to persist it.
        let todo_board = Arc::new(TodoBoard::new(
            store
                .as_ref()
                .map(|s| s.todos(&cfg.session_id).unwrap_or_default())
                .unwrap_or_default(),
        ));
        // **The slot a child of this session reads to join the tree** (R58). Declared
        // here, before the runner that holds it, and filled once `job_watch` exists
        // below — see `HarnessTaskRunner::tree_watch`.
        let tree_watch_slot: Arc<std::sync::Mutex<Option<Arc<crate::jobwatch::JobWatchers>>>> =
            Default::default();
        // **The slot a child of this session reads to reach the head's answers** — the ask
        // route's other half (R58), and the same shape as the watcher slot above. Seeded with
        // what THIS session inherited: a child's card is answered by its tree's root, so a
        // child that had its own answers would be answering into a hub no head is attached
        // to. A root has inherited nothing and writes its own in below, as its gate is built.
        let head_answers_slot: Arc<std::sync::Mutex<Option<Arc<crate::answers::Answers>>>> =
            Arc::new(std::sync::Mutex::new(parts.tree_head.clone()));
        // **The daemon's own server window, shared with the runner.** A spawn may ask for
        // `model: "local"` while this session is on a cloud provider; the child then plans
        // its compaction against the SERVER's window, which is this cell. See
        // `HarnessTaskRunner::local_window`.
        let local_window_cell: Arc<std::sync::Mutex<Option<Option<u64>>>> = Default::default();
        // **This session's point, in a cell its own runner can see** — the same shape as
        // the window above, and for the same reason one axis over: `base` is a clone taken
        // at open and nothing ever updates it, so a cell is the only way a spawn can read
        // the point the session is AT rather than the one it opened at. Seeded after the
        // mode is final (the placement override included), which is why it is built here
        // and not with the rest of the config. See `HarnessTaskRunner::point`.
        let point_cell: Arc<std::sync::Mutex<letibot_tools::mode::Mode>> =
            Arc::new(std::sync::Mutex::new(cfg.mode));
        let task_runner: Arc<dyn letibot_tools::builtins::task::TaskRunner> =
            Arc::new(HarnessTaskRunner {
                vocab: parts.vocab.clone(),
                wiring: parts.wiring.clone(),
                mode_store: parts.mode_store.clone(),
                registry: session_registry.clone(),
                base: cfg.clone(),
                tasks: parts.tasks.clone(),
                skills: parts.skills.clone(),
                lsp: parts.lsp.clone(),
                slots: parts.tree_slots.clone().unwrap_or_default(),
                // Filled below, once this harness has built its own `job_watch` (R58).
                tree_watch: tree_watch_slot.clone(),
                // Filled as this session's gate is built: a root's own answers, or the ones
                // its tree inherited (see `HarnessTaskRunner::head_answers`).
                head_answers: head_answers_slot.clone(),
                local_window: local_window_cell.clone(),
                point: point_cell.clone(),
                placed: Arc::new(std::sync::Mutex::new(Vec::new())),
                merge_store: Arc::new(std::sync::Mutex::new(None)),
            });
        // Cloned before `with_session_tools` takes it: `digest` folds its findings
        // through the same subagent runner `task` uses, so the two must be the
        // same object — a digest running on a second runner would be a subagent
        // tree the operator's `task_result` listing does not show.
        let digest_runner = task_runner.clone();
        // **And it is the session's HOLDER OF SUBAGENT HANDLES**, for the watchers and for
        // `job_kill` both. Cloned here because `task_runner` is MOVED into the registry on
        // the next statement — `digest_runner` above is the same object for the same
        // reason, and all three must see one runner: a second one would hand out handles
        // its sibling cannot collect or stop.
        let subagent_runner = task_runner.clone();
        registry = letibot_tools::with_session_tools(
            registry,
            todo_board.clone(),
            task_runner,
            parts.skills.clone(),
            parts.lsp.clone(),
        )
        .map_err(|e| HarnessError::Setup(format!("registering the todo tool: {e}")))?;
        registry
            .register(Box::new(letibot_tools::builtins::write::Write))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::edit::Edit)))
            .map_err(|e| HarnessError::Setup(format!("registering the write tools: {e}")))?;
        registry
            .register(Box::new(letibot_tools::builtins::bash::Bash))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobList)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobOutput)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobWait)))
            .and_then(|_| {
                registry.register(Box::new(
                    letibot_tools::builtins::jobs::JobKill::with_tasks(subagent_runner.clone()),
                ))
            })
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::monitor::Monitor)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::pkill::Pkill)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::ps::Ps)))
            .map_err(|e| HarnessError::Setup(format!("registering the exec tools: {e}")))?;
        let mut registry = letibot_tools::external_tools(registry, &external).map_err(|e| {
            HarnessError::Setup(format!("registering the outside-world tools: {e}"))
        })?;
        intent_tools::register_into(&mut registry, &intent_wiring)
            .map_err(|e| HarnessError::Setup(format!("registering the intent tools: {e}")))?;
        if let Some(t) = extra_tool {
            registry
                .register(t)
                .map_err(|e| HarnessError::Setup(format!("registering an extra tool: {e}")))?;
        }
        // The `flowy` door is `Sessions`' to seat, with the daemon's slot behind
        // it. A harness opened without `Sessions` — a test, a one-off — still has
        // roles that name the tool, so it gets the door with nothing behind it:
        // the tool then says "no seat" and names `/flowy login`, which is true.
        // **The session can see the harness it is inside.** Read-only: it reports
        // the banner, the `!` warnings, the turn and the attached heads, so a
        // model stops asking the operator to describe their own terminal. Seated
        // for every session including subagents — a subagent that cannot see its
        // own mode is the one most likely to guess at it — and registered here
        // because this is the layer that holds both the hub and the disclosures.
        let disclosure_slot: crate::facts::DisclosureSlot = Default::default();
        if registry.get("harness").is_none() {
            let facts = std::sync::Arc::new(crate::facts::DaemonFacts::new(
                hub.clone(),
                disclosure_slot.clone(),
            ));
            registry
                .register(Box::new(
                    letibot_tools::builtins::harness_view::HarnessView::new(facts),
                ))
                .map_err(|e| HarnessError::Setup(format!("registering the harness view: {e}")))?;
        }
        // **The session can read its own conversation, including the parts
        // compaction replaced.** `harness` declined to carry the transcript on
        // the grounds that a model re-reading its own context is spending it
        // twice; that holds right up until a compaction, after which the rows are
        // not in the context at all and the store is the only copy. The operator,
        // watching a session hand-build `sqlite3 … substr(item_json,1,120) …
        // ORDER BY seq DESC` against a hardcoded path: *"a disaster"*.
        //
        // Registered with a real source when there is a store and with
        // `NoTranscript` when there is not, rather than being left out: a tool
        // that is absent reads to a model as a capability this build lacks, and a
        // tool that refuses by name says which of the two it is.
        if registry.get("transcript").is_none() {
            let src: Arc<dyn letibot_tools::builtins::transcript::TranscriptSource> =
                match cfg.store.as_deref() {
                    Some(path) => {
                        match crate::transcript_source::StoreTranscripts::open(
                            path,
                            cfg.session_id.clone(),
                        ) {
                            Ok(s) => Arc::new(s),
                            // A store the daemon writes to but this reader cannot
                            // open is worth saying out loud once; the tool then
                            // refuses by name rather than reporting no history.
                            Err(e) => {
                                hub.publish(SessionEvent::Warning {
                                    code: "transcript_store".into(),
                                    detail: e,

                                    compaction: None,
                                });
                                Arc::new(letibot_tools::builtins::transcript::NoTranscript)
                            }
                        }
                    }
                    None => Arc::new(letibot_tools::builtins::transcript::NoTranscript),
                };
            let digest: Arc<dyn letibot_tools::builtins::digest::DigestRunner> = Arc::new(
                letibot_tools::builtins::digest::SubagentDigest::new(digest_runner),
            );
            // The gate's own record, by the same argument and from the same file.
            // The operator watched a session reach for `sqlite3 … SELECT
            // request_id, verdict, verdict_by, verdict_basis … FROM adjudication`
            // against a hardcoded path: *"the model shouldn't derive the storage,
            // its format, or what path it"* is at.
            let decisions: Arc<dyn letibot_tools::builtins::decisions::DecisionSource> =
                match cfg.store.as_deref() {
                    Some(path) => match crate::decision_source::StoreDecisions::open(
                        path,
                        cfg.session_id.clone(),
                    ) {
                        Ok(s) => Arc::new(s),
                        Err(e) => {
                            hub.publish(SessionEvent::Warning {
                                code: "decision_corpus".into(),
                                detail: e,

                                compaction: None,
                            });
                            Arc::new(letibot_tools::builtins::decisions::NoDecisions)
                        }
                    },
                    None => Arc::new(letibot_tools::builtins::decisions::NoDecisions),
                };
            registry
                .register(Box::new(
                    letibot_tools::builtins::transcript::TranscriptTool::new(src.clone()),
                ))
                .and_then(|_| {
                    registry.register(Box::new(letibot_tools::builtins::digest::DigestTool::new(
                        src, digest,
                    )))
                })
                .and_then(|_| {
                    registry.register(Box::new(
                        letibot_tools::builtins::decisions::DecisionsTool::new(decisions),
                    ))
                })
                .map_err(|e| {
                    HarnessError::Setup(format!("registering the transcript tools: {e}"))
                })?;
        }
        if cfg.parent_session_id.is_none()
            && cfg.seat != Seat::Runner
            && registry.get("flowy").is_none()
        {
            let (door, _slot) = letibot_flowy::Flowy::unattached();
            registry
                .register(Box::new(door))
                .map_err(|e| HarnessError::Setup(format!("registering the flowy door: {e}")))?;
        }

        // **The role comes from the SESSION when the session has one.**
        //
        // A daemon holds several sessions (`Sessions` keeps one `Harness` each, and a
        // `ToolRuntime` is per-`Harness`), so different tool sets in one process were
        // always expressible — every session resolved from the same `--role` only
        // because the store had nowhere to record anything else. `session.role` is
        // that place as of store v2.
        //
        // What this fixes today, before anything can *choose* a per-session role:
        // resuming a conversation used to re-seat it from whatever `--role` the
        // daemon happened to be started with. A coder session reopened by a daemon
        // started `--role planner` came back without `write`, and the only symptom
        // was a tool that was not there.
        //
        // An unparseable stored name is an ERROR and not a fallback. Falling back to
        // the daemon's role would re-seat the conversation silently, which is the
        // exact failure this column exists to remove.
        // `cfg.seat` already IS the session's role: it was taken from the store at
        // the top of this function, where it still had time to reach the backend.
        // One read, one seat — the two used to be resolved separately and could
        // disagree.
        let role = role_for(&cfg);
        let registry = registry
            .resolve_role(&role)
            .map_err(|e| {
                HarnessError::Setup(format!("seating the `{}` role: {e}", cfg.seat.as_str()))
            })?
            // The downgrade, applied to what is SEATED: a denied class's tools leave
            // the prompt, so the model is not told it has what the gate would refuse.
            .without_access(&cfg.downgrade.deny);
        let schemas = registry.schemas();
        let seated: Vec<String> = schemas.iter().map(|s| s.name.clone()).collect();

        use letibot_tools::schema::Access;
        let has_write_tools = schemas.iter().any(|s| s.access == Access::Write);
        let has_exec_tools = schemas.iter().any(|s| s.access == Access::Exec);
        let has_network_tools = schemas.iter().any(|s| s.access == Access::Network);
        // The gate is reachable from any class that is not unattended, not from
        // `write` alone. A planner with `say` and no writes still has a code path to
        // a question, and asking only about writes would call that session
        // unattended when it is not.
        let gated = has_write_tools || has_exec_tools || has_network_tools;

        // **The refusal that has to come before the banner.**
        //
        // A seat that can reach the gate and has nobody behind it is not a safe
        // default. It is a session that starts, prints, and then refuses every call
        // with `not_run` — one wasted turn to discover — while the model carries
        // tool definitions for capabilities it cannot use, which is the harness
        // lying to it in the stable prefix.
        //
        // The check is on the adjudicator's **own answer**, not on how it was
        // chosen: `NoAdjudicator::describe` begins `none`, and so does anything else
        // that has nothing behind it. A caller reaching `open_with` directly with
        // `NoAdjudicator` is the same state as an operator who attached nothing, and
        // making the two spellings one check is what keeps this from being routed
        // around by a test helper. `AdjudicatorChoice` has no value that means
        // nobody — see its `parse`, where `none` is refused with the reason.
        if gated
            && let Some(adj) = &adjudicator
            && adj.describe().starts_with("none")
        {
            let gated_tools: Vec<&str> = schemas
                .iter()
                .filter(|s| !s.access.is_unattended())
                .map(|s| s.name.as_str())
                .collect();
            return Err(HarnessError::Setup(format!(
                "the `{}` role seats {} tool(s) that must be adjudicated — {} — and the \
                 adjudicator attached is `{}`.\n\nThis refuses to open rather than \
                 opening and refusing every call. Failing closed is right and it is not \
                 a substitute for saying so before anything runs: a session in that \
                 state prints a banner, takes a prompt, and returns `not_run` — nobody \
                 decided — one turn later, having meanwhile told the model in its stable \
                 prefix that it has tools it cannot use.\n\nAttach an adjudicator, or \
                 seat `--role orchestrator`, which does not carry these tools at all.",
                cfg.seat.as_str(),
                gated_tools.len(),
                gated_tools.join(", "),
                adj.describe()
            )));
        }

        // **The point's prerequisites, checked before anything opens.**
        //
        // `Mode::check` refuses by name, says how to attach what is missing, and hands
        // back no weaker point. That last part is the whole of it: an operator who
        // asked for `writes allowed` and silently got `always-ask` has a banner saying
        // one thing and a session doing another, and finds out one wasted turn later.
        //
        // What is available is **read**, never assumed — the same discipline
        // `GateWiring` exists for. `WritableBackend` comes from the backend's own
        // answer; `ReachableAdjudicator` from the adjudicator's own `describe`, which
        // is how `NoAdjudicator` and a `HeadAdjudicator` with no head attached both
        // report themselves as nothing anybody can reach; `Confinement` from whether an
        // exec backend was actually built. `Oracle` is on nothing this build can
        // supply, which is why `automode` refuses here rather than being absent.
        let mut supplies: Option<(Vec<letibot_tools::mode::Prereq>, letibot_tools::mode::Seats)> =
            None;
        {
            use letibot_tools::mode::Prereq;
            let mut have: Vec<Prereq> = Vec::new();
            if backend_writable {
                have.push(Prereq::WritableBackend);
            }
            // A reachable adjudicator, by its own account. `HeadAdjudicator::describe`
            // says "none attached right now" when no head can answer, and that string
            // is the honest reading of *reachable* rather than *attached*.
            let reachable = adjudicator
                .as_ref()
                .map(|a| {
                    let d = a.describe();
                    !d.starts_with("none") && !d.contains("none attached")
                })
                // No adjudicator was handed in, so the harness builds one from
                // `cfg.adjudicator`, and both values it can build reach a person.
                .unwrap_or(true);
            if reachable {
                have.push(Prereq::ReachableAdjudicator);
            }
            if has_exec_tools && backend_confined {
                have.push(Prereq::Confinement);
            }
            // **The guard model, by the same fact the advisor is built from.**
            //
            // `cfg.oracle` is `Some` when an endpoint is known — `--oracle`, or
            // `[gatekeeper] endpoint` in the operator's providers.toml, which the
            // daemon reads at startup — and the advisor below is attached on exactly
            // that condition. Nothing used to push this, so `automode` could not
            // open however the box was configured: the operator's file said
            // `endpoint = "192.168.1.76:11500"`, `cfg.oracle` held it, and the
            // refusal told them to put in that file the line that was already in it.
            //
            // Measured 2026-09-15, and it BRICKED THE WORKSPACE rather than costing
            // a turn: `/mode automode` writes the project's row, and from then on
            // every daemon started in that project refused to open at all. A mode a
            // running daemon accepts and a restarted one cannot is the worst shape
            // this check can have — the state it guards against, one layer up.
            //
            // Reachability is deliberately not tested here. An endpoint that does
            // not answer is a guard that overruns its budget, and the gate already
            // abandons one that does; a TCP probe at open would make every session
            // start wait on another box being up.
            if cfg.oracle.is_some() {
                have.push(Prereq::Oracle);
            }
            // What the role actually seated, read off the resolved schemas rather than
            // off the role's name. A prerequisite is about a capability the session
            // will use: a read-only seat needs no writable backend however strict the
            // point is, and requiring one unconditionally stopped every read-only
            // session from opening at all.
            let seats = letibot_tools::mode::Seats {
                write: has_write_tools,
                exec: has_exec_tools,
                network: has_network_tools,
            };
            // **A child's point is its parent's, clamped by this seat** — the rule and
            // all of its reasoning live on `Mode::inherited_by`; what belongs here is why
            // it is applied at this line. `seats` and `have` are facts about the session
            // being opened (the schemas this seat actually seated, the backend it actually
            // built) and they do not exist any earlier, which is why the clamp is not
            // beside the mode-store lookup 700 lines up.
            //
            // It is a clamp and not a refusal because a child's point is not a choice
            // anybody makes: a seat that cannot carry what it inherited used to fail the
            // check below and the child never opened at all (`subagents dont work`).
            // Everybody else — `/mode`, `--mode`, a project row — still refuses by name
            // and is still handed no weaker point, which is the rule `check`'s own doc
            // states and this does not weaken.
            if cfg.parent_session_id.is_some() {
                let inherited = cfg.mode.inherited_by(&have, seats, cfg.seat.as_str());
                if let Some(why) = inherited.why() {
                    cfg.mode = inherited.mode();
                    // The row says where the point came from AND that it was clamped — a
                    // child silently at a different point than its parent is the state
                    // that made this a bug hunt in the first place.
                    mode_source = child_mode_source(Some(why));
                    // And the same sentence on the open notes, which is where a reader
                    // looks when the settings row is not in front of them.
                    notes.push(why.to_string());
                }
            }
            if let Err(why) = cfg.mode.check(&have, seats) {
                return Err(HarnessError::Setup(why));
            }
            // Kept for `/mode` mid-session: what this session CAN supply is fixed
            // at open — the backend, the oracle, the confinement, the seated
            // classes — and a point the operator moves to later is checked
            // against the same list, or the move refuses by name exactly as the
            // open would have.
            supplies = Some((have, seats));
        }

        let trail = Arc::new(TrailMirror::default());
        let injected: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let monitor_cursor = Arc::new(AtomicUsize::new(0));

        // **The gate, built here so the three seams cannot be skipped.** See
        // `open_with`'s docs for why this is not a parameter.
        let (gate, trail_installed, denials_surfaced): (Box<dyn Gate>, bool, bool) = match (
            adjudicator,
            gated,
        ) {
            // Nothing can reach it. `NoBoundary` is what every session that exists
            // today has, and keeping it means this file changed nothing for them.
            // A caller who attached an adjudicator to a read-only seat meant it, so
            // that falls through to the arm below and gets a real gate that nothing
            // ever calls.
            (None, false) => (Box::new(NoBoundary), false, false),
            (adj, _) => {
                let adj: Box<dyn Adjudicator> = match (adj, cfg.adjudicator) {
                    (Some(a), _) => a,
                    // **The answer path, installed as one act.** The sink has to be on
                    // the hub before the adjudicator can be asked anything, because
                    // the adjudicator posts to that hub and then waits — so these two
                    // lines are a pair, and separating them is a session that asks
                    // questions nothing can answer.
                    (None, AdjudicatorChoice::Head) => {
                        let answers = Arc::new(crate::answers::Answers::new());
                        hub.set_answer_sink(answers.clone());
                        // **And the tree can reach it.** A subagent's card is posted on a
                        // ROOT's hub and settled through that session's sink, so this is
                        // the one line that makes a child's ask answerable — recorded here,
                        // with the sink, because the two are one act (see the note below).
                        *head_answers_slot.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(answers.clone());
                        Box::new(crate::answers::HeadAdjudicator::new(hub.clone(), answers))
                    }
                    (None, AdjudicatorChoice::Console) => {
                        Box::new(letibot_tools::ConsoleAdjudicator::stdio(cfg.owner.clone()))
                    }
                    (None, AdjudicatorChoice::Model) => {
                        // **The model decides; when it cannot, the person does.**
                        // The operator's rule, after watching a call sit yellow and
                        // come back `not run` with nobody asked: an oracle timeout
                        // is a hand-off, not a refusal. The answer sink is
                        // installed as one act with the adjudicator that uses it —
                        // the same pair rule the Head arm below spells out.
                        let model =
                            model_adjudicator(&cfg, "`--adjudicator model`", Some(hub.clone()))?;
                        let answers = Arc::new(crate::answers::Answers::new());
                        hub.set_answer_sink(answers.clone());
                        // The tree's route to a person, recorded with the sink — a subagent's
                        // card reaches the same answer path the root's own escalation does.
                        *head_answers_slot.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(answers.clone());
                        Box::new(crate::answers::EscalateOnTimeout::new(
                            std::sync::Arc::from(model),
                            std::sync::Arc::new(crate::answers::HeadAdjudicator::new(
                                hub.clone(),
                                answers,
                            )),
                        ))
                    }
                };
                // **The guard model, attached whenever there is one to attach.**
                //
                // Built from `--oracle` alone and NOT from the mode, which is what
                // makes `/supervise` a toggle: the expensive half of supervision is
                // having a model reachable, and that is settled when the session
                // opens because the endpoint is a daemon argument. Whether it gets a
                // turn is a bool the operator moves whenever they like.
                //
                // `None` is not a failure. A session started without `--oracle` runs
                // exactly as it did before and `/supervise` refuses by name, which is
                // a better answer than refusing to start.
                let advisor: Option<std::sync::Arc<dyn Adjudicator>> = match &cfg.oracle {
                    Some(_) => {
                        Some(model_adjudicator(&cfg, "`--oracle`", Some(hub.clone()))?.into())
                    }
                    None => None,
                };
                let trail_for_gate = trail.clone();
                let g = AdjudicatedGate::new(adj)
                    .with_identity(cfg.session_id.clone(), cfg.owner.clone())
                    // Where this project sits. Without this the gate is at
                    // `UNSEEN_PROJECT` — always-ask — which is the fail-closed default
                    // and not what the operator recorded for this tree.
                    .with_mode(cfg.mode)
                    // opencode parity: exec follows the mode (so `allow-all` admits
                    // `bash`) rather than the operator's rule that exec always asks.
                    // Off for the confined seats, which keep the rule.
                    .with_exec_follows_mode(cfg.unconfined)
                    // opencode's `permission` config (LETIBOT_PERMISSION), so the
                    // allow/deny/ask rules govern before the mode — and a subagent
                    // inherits them.
                    .with_permission(downgraded_ruleset(
                        &cfg.permission,
                        &cfg.downgrade,
                        &schemas,
                    ))
                    // *Always allow* is written to the operator's file, so it is a
                    // preapproval every later daemon starts with.
                    .with_permission_sink(std::sync::Arc::new(|rule| {
                        let path = letibot_tools::permission::file_path()
                            .ok_or_else(|| "no $HOME, so no permission file".to_string())?;
                        letibot_tools::permission::append_to_file(&path, rule)
                    }))
                    // Layer A needs to know where it is standing. Undeclared means
                    // `ShellTrust::Unknown`, under which a **bare** command name is
                    // unresolved and the call is `not_run` — the fail-closed
                    // direction, and the honest one until
                    // `Surroundings::with_pinned_shell` can be called for real. See
                    // the refusal note in `surroundings_for`.
                    .with_surroundings(surroundings)
                    // §2. Without this the trail is `NotCollected` — *nobody
                    // looked*, which is not the same fact as an empty trail.
                    .with_trail_source(move |_call: &GateCall<'_>| trail_for_gate.trail())
                    // What the agent says it is doing, so the guard can connect the
                    // operator's words to a call that does not repeat them. Outside
                    // the trail's numbering by construction, so it can never be what
                    // an `ALLOW <n>` cites.
                    .with_agent_claim({
                        let t = trail.clone();
                        move || t.claim()
                    })
                    // §4b. Without this a denial reaches the model and stops there.
                    .with_denial_sink(Box::new(HubDenials::new(hub.clone())));
                let g = match advisor {
                    // **A point whose decider is the MODEL starts supervised.**
                    //
                    // `/supervise` is a toggle over a session whose mode leaves the
                    // deciding to a person. `automode` does not: `Decider::Model` is
                    // the whole content of the point, and a session that opened at
                    // automode with the guard attached and never consulted would be
                    // the banner-says-one-thing state this module refuses everywhere
                    // else. So the mode turns it on, and `--supervise` still turns it
                    // on for the points that do not.
                    Some(a) => g.with_advisor(a).start_supervised(
                        cfg.supervise || cfg.mode.decider == letibot_tools::mode::Decider::Model,
                    ),
                    None => g,
                };
                // **The corpus.** Without this the gate's rows are a `Vec` that dies
                // with the daemon, and every decision ever made here is gone —
                // including the labelled ones, which are the expensive kind. A second
                // connection to the session store, by its own design.
                let g = match corpus_sink.clone() {
                    Some(sink) => g.with_corpus_sink(sink),
                    None => g,
                };
                // **The shape cache, warm.**
                //
                // The cache lived in a `HashMap` on the gate, so it died with the
                // process — and the operator restarts a session exactly when one has
                // gone wrong, which is the worst possible moment to forget every
                // shape they approved. Measured on their own session: a restart put
                // them back to being asked about `cd <arg> ; grep -n <arg> <arg>`
                // again, which is the thing the cache exists to stop.
                //
                // Seeded from the store, bounded in SQL by `approved_shapes`: their
                // own approvals, admitted, `may_approve`, non-destructive, under THIS
                // workspace. The gate re-checks everything it can re-check at the
                // lookup, so this is a claim about the past and not a licence.
                //
                // A store that cannot answer is a note, never a refusal to start —
                // the cost of a cold cache is being asked, which is the safe side.
                let mut g = g;
                if let Some(st) = &store {
                    let ws = cfg.workspace.display().to_string();
                    match st.approved_shapes(&ws) {
                        Ok(rows) => {
                            let n = g.seed_shapes(rows);
                            if n > 0 {
                                notes.push(format!(
                                    "{n} command shape{} you approved in this project                                      before are remembered — they will not be asked again",
                                    if n == 1 { "" } else { "s" }
                                ));
                            }
                        }
                        Err(e) => notes.push(format!(
                            "the shape cache could not be warmed ({e}), so shapes you                              approved before will be asked again"
                        )),
                    }
                }
                (Box::new(g), true, true)
            }
        };

        // **The encoder, attached once for the life of the session.**
        //
        // `IntentSink` is a decorator on the runtime's own event sink and
        // constructing it is what tells the ledger an encoder exists — the flag is
        // `pub(crate)` precisely so that "an encoder was declared" and "an encoder
        // was wired" cannot be two different facts. It lives on the harness rather
        // than being rebuilt per round so that it is attached *before* the banner is
        // read, and so the call_id→name map it keeps spans a whole turn.
        // The background-job watcher: a job backgrounded this session settles
        // between turns, when nobody else is publishing, so the daemon watches
        // each one and publishes the settlement. `None` when the backend cannot
        // start processes — no host, no jobs, no threads. The sink is the hook
        // because the `Backgrounded` result is where a job id first exists.
        //
        // **And it is given the worker's bell (R7).** A settlement was reaching the
        // heads and nobody else: the model that could act on it had no route to a
        // background job's result but `job_wait`. The watcher now also queues a
        // completion and rings this bell, so the settlement arrives as an unprompted
        // turn of its own — the same route a fired monitor takes.
        // **Built whether or not this session can start a process.** `task` needs no
        // process host, so keying the watcher set on one would leave a read-only seat that
        // hands work to a child with no way to be told the child finished — the same
        // defect surviving in the sessions least able to notice it.
        let bell = Some(Arc::clone(session_registry.bell()));
        let job_watch = match backend.processes_arc() {
            Some(h) => JobWatchers::new(&h, &hub, bell),
            None => JobWatchers::watching_tasks(&hub, bell),
        };
        // **And the session's subagents settle through it too.** `task` returns the
        // same `Backgrounded` outcome `bash --background` does, so the sink below
        // already arms a watcher for it — and that watcher asked the host about a name
        // the host has never heard, answered `NeverStarted`, and spun on it until the
        // session closed: the completion never queued, the bell never rung, and a
        // thread leaked per `task` call. Giving the watchers the runner is what makes
        // the handle route to its own wait (R7's hop, for a subagent).
        let job_watch = Some(job_watch.with_tasks(&subagent_runner));
        // **And a child joins its PARENT's set** (R58, and the tree read as a supervision tree
        // in 2026-10-06). What joining gives it is three facts, and none of them is a ring
        // target any more:
        //
        //   * the `watching`/`settled` bookkeeping, so a tree does not watch or settle one
        //     handle twice;
        //   * **the level above** — whose queue what this session can no longer drain goes to,
        //     and whose id the ring for it names;
        //   * **the ROOT**, which the ask route posts a permission card to (`tree_root`), because
        //     that is the one session in a tree a head can be attached to.
        //
        // The RING is not among them: it names the session that OWNS a settlement — this one,
        // for what this session starts — and the daemon serves a session it does not hold by
        // handing the wake to the thread that does. Ringing the root instead is what put a
        // grandchild's notice somewhere nobody was looking; see `jobwatch`'s module header.
        // A root has no tree to join (`tree_watch` is `None` from `Parts::load`) and keeps its
        // own set, whose ring names itself.
        //
        // **The queue is NOT joined with it** (2026-10-05). A child's set keeps its own
        // completions, so what it backgrounds is drained by the child — and a settlement
        // that arrives after the child's turn is over is handed up one level by
        // `JobWatchers::stop` rather than read by the main session as its own.
        let job_watch = match (&parts.tree_watch, job_watch) {
            (Some(tree), Some(w)) => Some(w.shares_tree(tree)),
            (_, other) => other,
        };
        // **And this session's own set is what ITS children will join** (R58). Filled
        // here, after the join above, so the value a child reads is already the joined
        // one — which is how the ROOT (the session a permission card can be drawn by)
        // stays the tree's root at every depth without anything walking a parent chain.
        if let Some(w) = &job_watch {
            *tree_watch_slot.lock().expect("tree watch") = Some(Arc::clone(w));
            // **Where a settlement gets written down.** The watcher threads settle jobs on
            // their own thread and cannot be handed this harness's connection — a
            // `rusqlite::Connection` is `Send` and not `Sync` — so they are given the store's
            // PATH and open their own; `Store::path` documents why that is ordinary here and
            // why it answers `None` for an in-memory store. A run with no store records
            // nothing, which is the same answer it gives for todos.
            //
            // **This session's own id, not the tree's**, for the reason the queue is per
            // session: a job a child backgrounded is the child's row, in the child's session.
            if let Some(path) = store.as_ref().and_then(|s| s.path()) {
                w.record_jobs_into(path.to_path_buf(), cfg.session_id.clone());
            }
        }
        let tool_sink = JobWatchSink::new(
            IntentSink::new(intent.clone(), ToolLogSink::new(hub.clone())),
            job_watch.clone(),
        );

        // Read, never asserted. `is_writable` is the backend's own answer,
        // `describe` is the backend's own words, `Gate::describe` is the gate's, and
        // the access-class questions are answered by the schemas that were actually
        // seated a few lines above — not by which role was requested. Four instances
        // of one defect were paid for to learn that distinction.
        let wiring = GateWiring {
            adjudicator: gate.describe(),
            backend_writable,
            has_write_tools,
            external: letibot_tools::ExternalWiring::read(retrieval.as_ref(), &external, &schemas),
            role: role.name.clone(),
            seated,
            has_exec_tools,
            has_network_tools,
            backend: backend_described,
            denials_surfaced,
            trail_installed,
            // The ledger's own answer, not the fact that a line above meant to
            // attach one.
            intent_encoder: intent.encoder(),
            // A wake needs three things and this reads all three: a monitor
            // registry (so there is something to watch), the `monitor` tool seated
            // (so something can declare a watch), and `Sessions::watch` having
            // armed the waiter. The third is the daemon's and is stamped by
            // [`Harness::declare_monitor_wake`] when it arms — a harness driven
            // directly by a test has no daemon and honestly says `false`.
            monitor_wake: false,
            // **Read from the sink, not from the config.** `--store` being set is
            // not the same fact as a corpus connection having opened, and the
            // counts come from the table rather than from a count this process
            // kept — which would read as zero on a store full of earlier runs.
            corpus: corpus_counts,
        };

        let spiller = build_spiller(&cfg)?;
        // **The question a long tool asks before spending another minute.**
        //
        // `digest` folds two dozen parts and `job_wait` holds a deadline; for the
        // whole of either, this call is all the session is doing and nothing polls
        // the head. So the hub's own answer goes down into the tool runtime, and a
        // tool that is about to spend real time can find out that the operator is
        // waiting and stop. See `InvokeCtx::operator_waiting`.
        let waiting_hub = hub.clone();
        // **And the fact that closes R23.** `job_wait` on a job the daemon is already
        // watching returns at once rather than blocking on an answer that is in
        // flight — see `InvokeCtx::completion_delivered`. Wire to the same watcher
        // set the sink feeds, so "there is a watcher" and "the wait must not block"
        // are one fact read in two places rather than two facts that can disagree.
        // `None` when the backend cannot start processes: no host, no jobs, no
        // watcher, and every wait keeps its old behaviour.
        let delivering = job_watch.clone();
        let runtime = ToolRuntime::new(registry, backend)
            .with_spiller(spiller)
            .with_gate(gate)
            .with_operator_waiting(std::sync::Arc::new(move || waiting_hub.has_queued_prompt()))
            .with_completion_delivered(std::sync::Arc::new(move |job: &str| {
                delivering.as_ref().is_some_and(|w| w.delivering(job))
            }))
            // **The operator's own run can be ANSWERED** — see `crate::prompt`.
            //
            // Three reports and one object, and the object is installed on the registry
            // here because **this is the half that has the pipe**: the exec host was built
            // a few lines above, and the write end of the operator's stdin came out of the
            // job the tool spawned. The server's reader thread is the half that needs it,
            // and it cannot ask the worker — the worker is blocked inside the very command
            // that is asking.
            //
            // Keyed by session: a pipe belongs to one session's run, and a `!send` in one
            // session must never write into another's command.
            .with_operator_runs({
                let prompts = crate::prompt::Prompts::new(&cfg.session_id, hub.clone());
                session_registry.set_prompt(&cfg.session_id, prompts.clone());
                let runs = prompts.clone();
                std::sync::Arc::new(
                    move |what: letibot_tools::runtime::OperatorRun<'_>| match what {
                        letibot_tools::runtime::OperatorRun::Answerable {
                            job,
                            command,
                            stdin,
                        } => runs.opened(job, command, stdin),
                        letibot_tools::runtime::OperatorRun::Waiting { job, question } => {
                            runs.asking(job, question)
                        }
                        letibot_tools::runtime::OperatorRun::Ended { job } => runs.ended(job),
                    },
                )
            });

        let mut engine = TurnEngine::new(
            &parts.vocab,
            parts.wiring.renderer.as_ref(),
            parts.wiring.parser.as_ref(),
            cfg.endpoint.clone(),
            // A llama.cpp server we run: token ids in, per-stage cache accounting,
            // a wall-clock meter, and a structural prefix guarantee — so §18.1-I1's
            // post-flight assertion is allowed to run rather than being skipped.
            letibot_backend::BackendCaps::OWN_SERVER,
            cfg.model.clone(),
            cfg.sampling.clone(),
        )
        .map_err(|e| {
            HarnessError::Setup(format!(
                "the dialect does not fit this vocabulary: {e}. \
                 Every control token and stop literal must resolve to exactly one \
                 vocab entry; a mismatch here is a model/dialect pairing error, and \
                 it is raised now because at runtime it is silent."
            ))
        })?;
        // And this endpoint's media marker — see `engine_for`, whose construction this duplicates.
        engine.media_marker = cfg.media_marker.clone();

        let prefix = StablePrefix {
            system: cfg.system.clone(),
            tools_json: parts.wiring.tools_json(&schemas),
        };
        let dialect_sha = hex32(&parts.wiring.spec().template_sha);

        // A session already in the store is **resumed**, not opened a second time.
        //
        // Deciding it here rather than on a flag is what makes the two entry points
        // agree: `harnessd --session X` on a restart, and a head asking a running
        // daemon to resume X, are the same act and used to be two — the first of
        // which failed with a UNIQUE constraint on `session.id` after doing the
        // whole vocabulary load.
        let resumed = match (&store, &stored) {
            (Some(s), Some(st)) if st.items > 0 => {
                let transcript_id = st.transcript_id.clone().ok_or_else(|| {
                    HarnessError::Store(format!(
                        "session {} has {} row(s) but no transcript row to hang them on; \
                         this store was written by something else",
                        st.id, st.items
                    ))
                })?;
                Some((s, transcript_id))
            }
            _ => None,
        };

        let (transcript_id, session, persisted, resume, prefix, prefix_id) = match resumed {
            Some((s, transcript_id)) => {
                let loaded = s
                    .load_transcript(&transcript_id)
                    .map_err(|e| HarnessError::Store(e.to_string()))?;

                // **The refusal that has to stay.** The tokens in this transcript
                // were produced by one renderer against one vocabulary. Appending to
                // them with a different dialect would put two templates' bytes in
                // one prompt, and nothing downstream can see it: the chain still
                // verifies, because every row was hashed by whoever wrote it.
                let stored_sha = s
                    .stable_prefix_meta(&loaded.stable_prefix_id)
                    .map_err(|e| HarnessError::Store(e.to_string()))?
                    .map(|m| m.dialect_sha)
                    .unwrap_or_default();
                // **A rendering that cannot be reused is not a conversation that
                // cannot be continued.**
                //
                // This refused, and the reason it gave was sound about the path it
                // was on: a resume REPLAYS the stored tokens, and two renderers'
                // bytes in one prompt is a prompt no model was trained on, which
                // the hash chain cannot catch because every row is correctly hashed
                // by whoever wrote it.
                //
                // What it was not sound about is the data. `transcript_item` keeps
                // `item_json` beside `tokens`, and `item_json` is dialect-neutral —
                // a tool call is `{name, arguments}`, not `<function=…>` markup.
                // So the tokens are a CACHE of one rendering and the items are the
                // record, and the conversation can be rebuilt for this renderer.
                //
                // Measured on the session that made this visible
                // (s-1789462738453908838, 619 items, GLM -> Qwen dense, 2026-09-18):
                // every item re-rendered, 222176 stored tokens becoming 252128. The
                // cost is one cold prefill, once, and it is paid here rather than
                // discovered — the note says what it was before the head draws.
                //
                // It FORKS rather than rewriting: the old transcript keeps its
                // tokens and its chain, exactly as a compaction leaves what it
                // stopped carrying. Nothing stored is edited, so the append-only
                // triggers stay honest.
                if stored_sha != dialect_sha {
                    let store = s;
                    let items: Vec<letibot_transcript::TranscriptItem> =
                        loaded.items.iter().map(|(i, _, _)| i.clone()).collect();

                    let probe = engine
                        .open(&format!("{transcript_id}#reprefill-probe"), &prefix)
                        .map_err(|e| {
                            HarnessError::Setup(format!("rendering the new prompt: {e}"))
                        })?;
                    let rec = StablePrefixRecord {
                        dialect_sha: dialect_sha.clone(),
                        system: prefix.system.clone(),
                        tools_json: prefix.tools_json.clone(),
                        tokens: probe.ledger.prefix_tokens().to_vec(),
                        h_init: probe.ledger.h_init(),
                        vocab_source: cfg.vocab_gguf.display().to_string(),
                    };
                    drop(probe);
                    let new_prefix_id = store
                        .put_stable_prefix(&rec)
                        .map_err(|e| HarnessError::Store(e.to_string()))?;

                    // `#tN`, N counted the way `fork_to_summary` counts it, so the
                    // two forks cannot collide on an id.
                    let n: i64 = store
                        .connection()
                        .query_row(
                            "SELECT COUNT(*) FROM transcript WHERE session_id = ?1",
                            [cfg.session_id.as_str()],
                            |r| r.get(0),
                        )
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    let new_id = format!("{}#t{}", cfg.session_id, n);

                    let mut rebuilt = engine
                        .open(&new_id, &prefix)
                        .map_err(|e| HarnessError::Setup(format!("opening the fork: {e}")))?;
                    // Each item rendered against the history as it stood before it,
                    // which is `render_incremental`'s contract and what keeps the
                    // ledger rows aligned with the items.
                    rebuilt
                        .append_items(&engine, &items, &mut letibot_turn::events::NullSink)
                        .map_err(|e| {
                            HarnessError::Setup(format!(
                                "re-rendering this conversation under {}: {e}",
                                parts.wiring.spec().name
                            ))
                        })?;

                    store
                        .put_fork(
                            &new_id,
                            &cfg.session_id,
                            &new_prefix_id,
                            &transcript_id,
                            items.len() as u32,
                        )
                        .map_err(|e| HarnessError::Store(e.to_string()))?;

                    // Written before the head attaches, so a daemon that dies now
                    // leaves a fork that resumes rather than one that has to be
                    // rebuilt again.
                    let rows = rebuilt.ledger.rows();
                    for i in 0..rows.len() {
                        let tokens = rebuilt.ledger.item_tokens(i).ok_or_else(|| {
                            HarnessError::Store(format!("no tokens for re-rendered row {i}"))
                        })?;
                        store
                            .append_item(&new_id, i as u32, &rebuilt.items[i], &rows[i], tokens)
                            .map_err(|e| {
                                HarnessError::Store(format!("re-rendered row {i}: {e}"))
                            })?;
                    }

                    let before: usize = loaded.items.iter().map(|(_, _, t)| t.len()).sum();
                    notes.push(format!(
                        "this conversation was recorded under dialect template {} and this \
                         daemon renders {}. Its {} item(s) were RE-RENDERED for this one — \
                         the stored tokens are a cache of the other rendering, the items \
                         themselves are the record, and nothing was detokenized to do it. \
                         {} token(s) became {}, which is one cold prefill, once, on the next \
                         turn. The old transcript {} keeps its tokens and its chain; this is \
                         a fork, and nothing stored was rewritten.",
                        &stored_sha[..16],
                        &dialect_sha[..16],
                        items.len(),
                        before,
                        rebuilt.ledger.len(),
                        transcript_id,
                    ));

                    let rows = rebuilt.ledger.rows().len();
                    let report = ResumeReport {
                        transcript_id: new_id.clone(),
                        rows,
                        tokens: rebuilt.ledger.len(),
                        head: rebuilt.ledger_head(),
                        workspace: cfg.workspace.display().to_string(),
                        notes: std::mem::take(&mut notes),
                    };
                    // Every row was written above, so none is pending.
                    (
                        new_id,
                        rebuilt,
                        rows,
                        Some(report),
                        prefix.clone(),
                        new_prefix_id,
                    )
                } else {
                    let session = Session::restore(&loaded).map_err(|e| {
                        HarnessError::Store(format!("session {}: {e}", cfg.session_id))
                    })?;

                    // The *fact*, not a proxy for it: render this daemon's stable prefix
                    // and compare the tokens with the ones the session is carrying. Equal
                    // means the vocabulary and the renderer agree, whatever the recorded
                    // GGUF path says; different means they do not, and the session keeps
                    // the prefix it was created with — which is correct and has to be
                    // said, because the operator's `--system` change did not take effect
                    // in this session and nothing else would tell them.
                    let fresh = engine
                        .open(&format!("{transcript_id}#probe"), &prefix)
                        .map_err(|e| HarnessError::Setup(format!("rendering the prefix: {e}")))?;
                    if fresh.ledger.prefix_tokens() != session.ledger.prefix_tokens() {
                        notes.push(format!(
                            "this session keeps the stable prefix it was created with \
                         ({} tokens, h_init {}). The prefix this daemon would render now \
                         is {} tokens — a changed system prompt, tool set or effort level. \
                         Rewriting message 0 is what forces a full cold re-prefill, so it \
                         is not done; start a new session to pick up the change.",
                            session.ledger.prefix_len(),
                            &hex32(&session.ledger.h_init())[..16],
                            fresh.ledger.prefix_len(),
                        ));
                    }

                    // The prefix the session's own tokens came from — the store's
                    // record, not the daemon's current render. A compaction fork keeps
                    // it, exactly as the resume itself does.
                    let own = s
                        .stable_prefix_record(&loaded.stable_prefix_id)
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    let own_prefix = StablePrefix {
                        system: own.system,
                        tools_json: own.tools_json,
                    };

                    // **What the MODEL can call is the prefix, not the registry.**
                    //
                    // The tool schemas live in the stable prefix, and a resume replays
                    // the stored one — it must, the stored tokens were produced under
                    // it. The registry, meanwhile, is rebuilt from THIS daemon's flags.
                    // When the two disagree the session has tools the conversation has
                    // never been told about, and every disclosure below is computed
                    // from the registry: the banner announced `bash` and
                    // `Access: exec` at a session whose prompt lists nine tools and no
                    // shell, so the model never called it and the operator spent an
                    // hour on "still no exec" while the banner said exec was seated.
                    //
                    // Named here, where both halves are in hand. This does not refuse:
                    // the conversation is intact and every tool the PREFIX declares
                    // still works. What it may not do is let the disclosure claim the
                    // difference away.
                    {
                        let now = parts.wiring.tools_json(&schemas);
                        if now != own_prefix.tools_json {
                            // Both shapes a tool schema is rendered in: OpenAI's
                            // `{function:{name}}` and the bare `{name}`. A schema whose
                            // name cannot be read is left out of the diff rather than
                            // guessed at — the sentence below names what it is sure of.
                            let named = |t: &[String]| -> std::collections::BTreeSet<String> {
                                t.iter()
                                    .filter_map(|j| {
                                        let v: serde_json::Value = serde_json::from_str(j).ok()?;
                                        let n = v
                                            .pointer("/function/name")
                                            .or_else(|| v.get("name"))?
                                            .as_str()?;
                                        Some(n.to_string())
                                    })
                                    .collect()
                            };
                            let (was, is) = (named(&own_prefix.tools_json), named(&now));
                            let added: Vec<&String> = is.difference(&was).collect();
                            let gone: Vec<&String> = was.difference(&is).collect();
                            let mut say = String::from(
                                "this session's PROMPT carries the tool list it was created with,                              and this daemon seats a different one. A resume replays the                              stored prefix, so what the model can actually call is the                              stored list",
                            );
                            if !added.is_empty() {
                                say.push_str(&format!(
                                " — seated here but NOT in this conversation's prompt, so the                                  model cannot call them: {}",
                                added
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                            }
                            if !gone.is_empty() {
                                say.push_str(&format!(
                                " — in the prompt but not seated here, so a call to them                                  refuses: {}",
                                gone.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                            ));
                            }
                            say.push_str(
                            ". `/reseat` rebuilds the prompt from what is seated now,                              forking the conversation onto it and carrying it across as it                              is; a new session gets the seated list from the start.",
                        );
                            notes.push(say);
                        }
                    }

                    let rows = session.ledger.rows().len();
                    let report = ResumeReport {
                        transcript_id: transcript_id.clone(),
                        rows,
                        tokens: session.ledger.len(),
                        head: session.ledger_head(),
                        workspace: cfg.workspace.display().to_string(),
                        notes: std::mem::take(&mut notes),
                    };
                    (
                        transcript_id,
                        session,
                        rows,
                        Some(report),
                        own_prefix,
                        loaded.stable_prefix_id,
                    )
                }
            }
            None => {
                let transcript_id = format!("{}#t0", cfg.session_id);
                let session = engine
                    .open(&transcript_id, &prefix)
                    .map_err(|e| HarnessError::Setup(format!("opening the session: {e}")))?;
                let mut prefix_id = String::new();
                if let Some(s) = &store {
                    let rec = StablePrefixRecord {
                        dialect_sha: dialect_sha.clone(),
                        system: prefix.system.clone(),
                        tools_json: prefix.tools_json.clone(),
                        tokens: session.ledger.prefix_tokens().to_vec(),
                        h_init: session.ledger.h_init(),
                        vocab_source: cfg.vocab_gguf.display().to_string(),
                    };
                    prefix_id = s
                        .put_stable_prefix(&rec)
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    // `INSERT OR IGNORE`-shaped by hand: a session row may already be
                    // here with no rows behind it — the daemon that made it never got
                    // a prompt — and re-opening it is a resume of an empty
                    // conversation, not a collision.
                    if stored.is_none() {
                        s.put_session(&SessionRecord {
                            id: cfg.session_id.clone(),
                            title: Some(cfg.title.clone()).filter(|t| !t.is_empty()),
                            model_id: cfg.model.clone(),
                            dialect_sha,
                            workspace_root: cfg.workspace.display().to_string(),
                            owner: cfg.owner.clone(),
                            // Record it, so a later resume seats what this
                            // conversation was built with rather than whatever the
                            // daemon is running as then.
                            role: Some(cfg.seat.as_str().to_string()),
                            approvers: vec![],
                            parent_session_id: cfg.parent_session_id.clone(),
                        })
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    }
                    if !transcript_exists(s, &transcript_id) {
                        s.put_transcript(&transcript_id, &cfg.session_id, &prefix_id)
                            .map_err(|e| HarnessError::Store(e.to_string()))?;
                    }
                }
                (transcript_id, session, 0, None, prefix, prefix_id)
            }
        };

        // **The window this session plans its compaction against, written down.** See
        // `StoredSession::context_window`: the number was otherwise knowable only at the
        // moment it was used, and a child that ran past its wall left nothing behind saying
        // which wall it had. Written for a root and a child alike — `cfg.context_window` is
        // the resolved number either way, and for a child it is the one the spawn chose for
        // the model that answers IT rather than its parent's.
        //
        // After the match above, because the row it updates is created inside it.
        if let Some(store) = &store
            && let Err(e) = store.set_window(&cfg.session_id, cfg.context_window)
        {
            // **A bookkeeping write failing is not a reason to refuse a session**, and it is
            // not silent either: `notes` is what the operator reads at startup, and this is
            // the one place a reader would look for why the store cannot answer the question.
            notes.push(format!(
                "the window this session plans against could not be written to the store \
                 (`{e}`). It is in force for this run; after a restart nothing on disk says \
                 which window it was."
            ));
        }

        // **The trail is seeded from what was restored, and it says what it lost.**
        //
        // A resumed session's `User` rows are indistinguishable in the store: the
        // operator's prompts, a head's steering and this harness's own notices are
        // all `TranscriptItem::User`, and nothing recorded which was which. Seeding
        // them as the operator's words is the permissive direction for
        // authorisation, so it is a resume note rather than a silent property — the
        // operator can see that this session's adjudications are reading a trail
        // whose speakers were inferred, and start a fresh session if that matters.
        let mut resume = resume;
        if resume.is_some() {
            trail.seed(&session.items);
            let user_rows = session
                .items
                .iter()
                .filter(|i| matches!(i, TranscriptItem::User { .. }))
                .count();
            if let Some(r) = resume.as_mut()
                && gated
            {
                r.notes.push(format!(
                    "the authorisation trail was seeded from {user_rows} restored user \
                     row(s), all read as the OPERATOR's words and none with a clock. The \
                     store does not record which user rows this harness injected — a \
                     steering message and a §5.7 notice are the same shape as a prompt — \
                     so a rebuilt trail is the permissive reading. Everything said from \
                     now on is recorded with its real speaker and its real distance."
                ));
            }
        }

        // **The standing choice is resolved by the DAEMON, not here.**
        //
        // This used to read `providers.toml` itself, which made opening a harness
        // depend on a file outside the config it was handed — and every test that
        // opens one inherited the operator's live standing choice. Caught by two
        // `compact` tests that assert a DEAD endpoint and got real answers from
        // deepseek instead: four turns and ~22k prompt tokens, billed to the
        // operator, by `cargo test`.
        //
        // `harnessd`'s own startup resolves it into `cfg.provider` before any
        // session exists, and `Sessions` clones that config for every session it
        // opens — so a real daemon behaves exactly as before and a `Config` built
        // in a test carries only what the test put in it.
        let provider: Option<Box<dyn letibot_backend::MessagesBackend>> = match &cfg.provider {
            None => None,
            Some(pc) => Some(build_provider(pc, &cfg.sampling).map_err(HarnessError::Setup)?),
        };
        // **The ledger-to-provider ratio survives a restart, because it is already
        // in the store.**
        //
        // `Config::ledger_scale` is measured at the end of each metered turn and
        // lives in memory, so a restart threw it away and everything that plans
        // against the window — `should_compact`, `room_for_next_turn`, the
        // `/reseat` carry check — fell back to comparing ledger tokens against a
        // provider window until the next turn measured it again.
        //
        // That window is not a rare one: it is exactly the sequence this daemon
        // asks for. Restart to pick up new tools, `/reseat` to get them into
        // message zero — and the re-seat is the first thing that runs, before any
        // turn. The operator hit it within minutes of the restart:
        // *"reseat_unchecked — this session has not taken a metered turn yet"*.
        //
        // Nothing has to be re-measured. `context_tokens` on the session row IS
        // the provider's count for the last prompt, written by `persist_context`
        // at the end of the turn that measured it, and `context_ledger` beside it
        // is this box's count for that SAME prompt (v12) — so the pair is
        // recovered rather than recomputed, and no model is asked anything.
        //
        // **Both halves, or neither.** The paragraph that used to stand here said
        // the ledger "is the one just rebuilt — the conversation has not changed
        // since", and that was an assumption rather than a measurement: it holds
        // only when nothing was appended between the last turn and this restart.
        // The gate below asks the row instead of assuming.
        // **And only when the pair measures THIS conversation.**
        //
        // A ratio needs both counts from the same prompt, and the row now records
        // them together. Without this check the pair was reconstructed from
        // `session.ledger.len()` — TODAY's ledger — which is the same conversation
        // only if nothing was appended since the measurement. At a resume it
        // usually is not, and the error lands the dangerous way: a count from a
        // SMALLER conversation divided into a BIGGER ledger overstates the ratio,
        // so the window grows and the session compacts too late rather than too
        // early.
        //
        // Measured 2026-10-02, on the session this check would have saved: a store
        // holding 940,211, measured on a ~950k-token conversation, was paired with
        // a rebuilt ledger of 1,486,369 — 0.63 where the truth was 1.016. The
        // window came out 1,580,888 for a session already 1.46M tokens deep, so it
        // never compacted and died at the provider's 400 on every attempt.
        //
        // A row with no `context_ledger` was written before the column existed and
        // is REFUSED rather than guessed at. Unverifiable is not the same as wrong,
        // but the two are the same as unmeasured, and the unscaled window is the
        // honest answer for both — it compacts early, which is the direction nobody
        // loses a session to. See `Config::tokens_are_unscaled`.
        if cfg.provider.is_some()
            && cfg.ledger_scale.is_none()
            && let Some(store) = &store
            && let Ok(Some(row)) = store.session(&cfg.session_id)
            && let Some(scale) =
                recovered_scale(row.context_tokens, row.context_ledger, session.ledger.len())
        {
            cfg.ledger_scale = Some(scale);
        }
        // **Fill the harness view's slot, now that there is something to disclose.**
        // The tool was registered before the gate and the backend existed — it had
        // to be, to be in the prompt — and this is the first point where the
        // disclosures it reports can be computed.
        *disclosure_slot.lock().unwrap_or_else(|e| e.into_inner()) = cfg
            .disclosures(&wiring)
            .into_iter()
            .map(|d| (d.subject, d.state, d.detail))
            .collect();
        let mut h = Harness {
            // no prompt yet: the first `TurnStarted` after a prompt stamps it
            turn_began_ms: None,
            session_registry: session_registry.clone(),
            mode_source,
            wiring,
            cfg,
            engine,
            session,
            runtime,
            hub,
            store,
            corpus: corpus_sink,
            transcript_id,
            prefix,
            prefix_id,
            persisted,
            system_updates: 0,
            last_turn_id: String::new(),
            resumed: resume,
            compacting: false,
            render: parts.wiring.clone(),
            supplies,
            open_notes: notes,
            new_title: None,
            bang_seq: 0,
            trail,
            todos: todo_board,
            todos_version: 0,
            intent,
            injected,
            monitors,
            monitor_cursor,
            tool_sink,
            job_watch,
            // The same `Arc` the tool registry has, cloned before it was moved in — see the
            // field: an interrupt has to be able to stop this session's children.
            subagents: subagent_runner,
            provider,
            // Filled on the first switch away from local, never at open: a session
            // that started on a provider has no local window to go back to, and
            // `None` here says exactly that.
            local_window: None,
            local_window_cell: local_window_cell.clone(),
            point_cell: point_cell.clone(),
        };
        h.publish_settings();
        h.publish_jobs();
        if h.resumed.is_some() {
            // **The RESUME's own sentence, and it is not the carry's.** The operator read the
            // carry's wording here and asked the obvious question — *"why it was decided to carry
            // the conversation to the new prompt?"* — about an operation that rebuilds no prompt:
            // a resume re-announces stored rows so the head can draw the conversation it already
            // had. Naming the wrong operation is the same defect the `Filling` event was written
            // to end, one layer up: an indicator must be the fact, not a rendering of the fact.
            // ...and it restores the CONVERSATION, which a compaction splits across
            // transcripts: the tail of the ones before comes back too, up to the view's bound.
            let before = h.ancestor_tail();
            h.republish_after("restoring the stored conversation", before);
        }
        // **R6: a session whose id marks an opencode conversation reads it in.**
        //
        // `oc-` is the namespace mark and the rest is opencode's opaque id
        // (`ses_f68f5d…`). Only for a session that is **not** a resume: re-running the
        // same `--session oc-<id>` finds the stored session and resumes it rather than
        // importing a second copy, which is the idempotency rule. The import never
        // fails the session — a missing database or id is a sentence on the log and
        // the head stays up, because by the time it is read the head is already a
        // working head with a screen.
        if h.resumed.is_none()
            && let Some(oc_id) = h.cfg.session_id.strip_prefix("oc-").map(str::to_string)
        {
            h.import_opencode(&oc_id);
        }
        Ok(h)
    }

    /// **Read an opencode conversation into this session (R6).**
    ///
    /// The reading is `letibot-opencode`'s; this is the half that turns its rows into
    /// **this** session's rows and keeps the screen live while it does. It is
    /// deliberately not allowed to fail the session: a missing database or a missing id
    /// is a sentence on the log, and the head stays up.
    ///
    /// # Why the read is on the worker and the screen is still up first
    ///
    /// `Session::append_items` is the one writer and it is the worker's, so the reading
    /// happens here. What makes that *"the UI is up before anything is read"* rather
    /// than a startup dependency is that the **head attaches to the registry's hub, not
    /// to this harness**: the session is registered — and attachable — before the worker
    /// opens it, so a head joining now gets an empty snapshot and every row as a live
    /// event. Rows land as they are read because each is appended and published as the
    /// reader reaches it, and `ImportProgress` reports the reader's own count so the bar
    /// is the fact and not a rendering of it.
    fn import_opencode(&mut self, oc_id: &str) {
        let Some(path) = letibot_opencode::default_db_path() else {
            self.import_note(
                "import_no_db",
                "no $HOME, so no opencode database path to read. The session stays up and \
                 empty."
                    .into(),
            );
            return;
        };
        let source = match letibot_opencode::Source::open(&path) {
            Ok(s) => s,
            Err(e) => {
                self.import_note(
                    "import_no_db",
                    format!(
                        "{e}\n\nThe session stays up: prompt it like any other and it runs on \
                         this head's own model."
                    ),
                );
                return;
            }
        };
        let tree = match source.tree(oc_id) {
            Ok(t) => t,
            Err(e) => {
                self.import_note(
                    "import_no_session",
                    format!("{e}\n\nThe session stays up and empty."),
                );
                return;
            }
        };

        // **Rule 1, said where a reader will see it.** These are another agent's rows,
        // under another provider; a reader who cannot tell whose they are cannot weigh
        // them. The origin — the id, the directory, the provider/model — goes on the log
        // the head reads.
        self.import_note("imported", tree.root.origin());
        let total = tree.parts;
        const WHAT: &str = "importing an opencode conversation";
        self.filling(WHAT, "parts", 0, total);

        let mut failure: Option<String> = None;
        let mut since_persist = 0u64;
        let report = source.read_tree(oc_id, &mut |ev| match ev {
            letibot_opencode::Event::Progress { done, .. } => {
                // Throttled: one tick per 64 parts is a live bar, and one per part is
                // thousands of publishes for a quarter-second read.
                if done % 64 == 0 {
                    self.filling(WHAT, "parts", done, total);
                }
            }
            letibot_opencode::Event::Row(r) => {
                if failure.is_some() {
                    return;
                }
                if let Err(e) = self.append_imported(&[r.item]) {
                    failure = Some(e.to_string());
                    return;
                }
                since_persist += 1;
                if since_persist >= 512 {
                    since_persist = 0;
                    if let Err(e) = self.persist() {
                        failure = Some(e.to_string());
                    }
                }
            }
            letibot_opencode::Event::Scrap(s) => self.import_note("import_scrap", s.said),
        });

        match (failure, report) {
            (Some(e), _) => self.import_note(
                "import_failed",
                format!(
                    "the import stopped part-way: {e}. What arrived is on the screen and in \
                     the store; the rest did not."
                ),
            ),
            (None, Err(e)) => self.import_note(
                "import_failed",
                format!(
                    "reading opencode's database stopped part-way: {e}. What arrived is on the \
                     screen and in the store; the rest did not."
                ),
            ),
            (None, Ok(report)) => {
                let _ = self.persist();
                // The last tick clears the head's line; the summary is the durable
                // residue, and it carries the spend rule 2 is about.
                self.filling(WHAT, "parts", total, total);
                self.import_note("imported_summary", report.summary());
            }
        }
    }

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
        let mut quiet = letibot_tools::events::NullToolSink;
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
    pub fn run_operator_shell(&mut self, line: &str, who: &str) -> Result<(), String> {
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
        // The runtime's own events go to a null sink, exactly as the door's do: the
        // transcript row is the durable record and every head draws *it*. What the
        // tool emits here is a `ToolStarted`/`ToolProgress` pair for a call no head
        // proposed — and for a command the operator is watching the composer over.
        let mut quiet = letibot_tools::events::NullToolSink;
        // `turn_id` empty on purpose, as the door's runs are: this call belongs to no
        // turn, and a head that saw a turn id would draw the row inside a turn that
        // did not propose it.
        let result = self.runtime.invoke_operator("", &call, &mut quiet);
        let payload = result.render();
        // **What the model will read** — the same disclosure the door's note makes,
        // for the same reason: the price is on the screen while the operator can still
        // do something about it, rather than at the next compaction.
        let read = payload.len();
        let spilled = match &result.spill {
            Some(s) => format!(
                "; {} byte(s) were produced and the rest went to the spill store \
                 (`read_spill hash={}`)",
                s.full_bytes, s.hash
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
                call_id,
                name: "bash".to_string(),
                outcome: result.outcome.clone(),
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
        self.run_rounds()
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

    /// One progress tick for a named operation, from the daemon's own counter.
    ///
    /// The head draws a bar from this and from nothing else: it does not derive one
    /// from body-less rows, because that would mean *inferring* the operation from
    /// the symptom, which is the defect this event exists to end.
    fn filling(&self, what: &str, unit: &str, done: u64, total: u64) {
        self.hub.publish(letibot_sessionlog::SessionEvent::Filling {
            what: what.to_string(),
            unit: unit.to_string(),
            done,
            total,
        });
    }

    /// Put the restored conversation back on the session's log.
    ///
    /// A resumed harness holds the transcript; the **hub** does not, and a head
    /// attaching to it would be told it had joined an empty session with a 39,384-token
    /// prompt. So each restored row is announced exactly as a live one is —
    /// `TranscriptAppended`, then the body through `Hub::record_item` — and both the
    /// snapshot path and the already-attached path get it for free, because they are
    /// the paths a live append already takes.
    ///
    /// These are **not** re-renders and not re-runs: no turn events, no metrics, no
    /// `TurnFinished`. What a head shows after a resume is the conversation, not a
    /// replay of the turns that produced it, and inventing turn boundaries here would
    /// put timings on the screen that no clock measured.
    ///
    /// **And it says what it is doing** — in the caller's words, because the callers are
    /// different operations. A resume re-announces stored rows so an attaching head can draw
    /// the conversation; a re-seat carries that conversation onto a rebuilt prompt. Both
    /// announce thousands of rows and both are a real wait on a real session, but only one of
    /// them rebuilds a prompt, and a single sentence for the two told the operator they were
    /// watching the other one. That is why `what` is a parameter: **the layer that owns the
    /// fact states it**, which is this event's own rule.
    ///
    /// One `Filling` per row is the counter; the head draws it whenever the total is large
    /// enough to be worth a bar.
    /// **The rows a compaction put behind the current transcript, as many as the view holds.**
    ///
    /// The operator, after a restart: *"conversation was gone — only a small recent portion was
    /// displayed"*. A resume republished `self.session.ledger` — the CURRENT transcript — and a
    /// compaction makes the current transcript a new one: the summary and what came after it. So
    /// a session compacted twenty times came back as its last 791 rows, `items_dropped` said
    /// nothing came before them, and no head could scroll above them. Before the restart it
    /// could: the hub had lived through the compaction and still held the older rows.
    ///
    /// So a resume walks the chain, newest parent first, and takes rows from the END of each
    /// until the view's own bound is met — the same bound the view trims to, so this never
    /// publishes a row the view would drop. The ledger and the model's prompt are untouched:
    /// this is what the SCREEN is given, which is the conversation, not what the model reads,
    /// which is the summary. The first row of the current transcript is the compaction's own
    /// system row, so the seam between the two says what it is without a mark of ours.
    fn ancestor_tail(&self) -> Vec<(String, TranscriptItem, [u8; 32])> {
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        let bound = letibot_sessionlog::view::ViewBounds::default().items;
        let mut room = bound.saturating_sub(self.session.ledger.rows().len());
        let mut chunks: Vec<Vec<(String, TranscriptItem, [u8; 32])>> = Vec::new();
        let mut cursor = store.parent_of(&self.transcript_id).ok().flatten();
        while room > 0 {
            let Some(id) = cursor.take() else { break };
            let Ok(t) = store.load_transcript(&id) else {
                break;
            };
            let n = t.items.len();
            let take = n.min(room);
            chunks.push(
                t.items
                    .into_iter()
                    .skip(n - take)
                    .map(|(item, row, _)| (row.item_id, item, row.h_k))
                    .collect(),
            );
            room -= take;
            cursor = t.parent_transcript_id;
        }
        // Collected newest-parent first; published oldest first, so the view reads in order.
        chunks.into_iter().rev().flatten().collect()
    }

    fn republish_after(&self, what: &str, before: Vec<(String, TranscriptItem, [u8; 32])>) {
        let current = self
            .session
            .ledger
            .rows()
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                self.session
                    .items
                    .get(i)
                    .map(|item| (row.item_id.clone(), item.clone(), row.h_k))
            });
        let rows: Vec<(String, TranscriptItem, [u8; 32])> =
            before.into_iter().chain(current).collect();
        let total = rows.len() as u64;
        for (i, (item_id, item, h_k)) in rows.iter().enumerate() {
            // **ONE PROGRESS TICK PER ROW IS A FLOOD, AND THE FLOOD IS WHAT BREAKS THE BAR.**
            //
            // This published ~2000 `Filling` events as fast as a loop can go — on top of a
            // `TranscriptAppended` and a `record_item` for every row — and a head's queue has a
            // cap. Over the cap the hub DEMOTES the head rather than blocking
            // (`hub.rs:1369`): the queue is cleared, `needs_resync` is set, and from then on
            // *"Already demoted; its queue is going to be thrown away"* — so the ticks stop
            // arriving. MEASURED: the operator watched the bar freeze and then vanish —
            // *"it was back and kinda frozen at 44 — the bar wasn't moving for seconds and then
            // disappeared"* — on a head that finished holding all 2039 items, i.e. one that was
            // demoted and handed a snapshot mid-restore.
            //
            // So the bar froze at the last tick it heard and never saw the rest, because the
            // event stream carrying it had been dropped. **The thing that feeds the progress
            // indicator was killing the progress.** A bar is read as a fraction, not as a row
            // count, so a tick every `FILLING_STRIDE` rows loses the reader nothing and costs
            // the head's queue ~30 events instead of ~2000.
            //
            // The first and last rows always tick, so a bar appears promptly and — with the
            // completion below — ends by fact.
            //
            // **The stride is on the TICK, never on the row.** `7614785` put a `continue` here,
            // ahead of the publish, so it skipped the row as well as the tick: a restore of 791
            // rows announced 13 of them — row 0, every 64th, and the last. The operator after
            // the next restart: *"conversation was gone — only a small recent portion was
            // displayed"*. The head held 13 restored rows plus the live turns since, and nothing
            // said anything was missing, because nothing had been dropped: it was never sent.
            if i % FILLING_STRIDE == 0 || (i as u64 + 1) == total {
                self.filling(what, "rows", i as u64 + 1, total);
            }
            self.hub
                .publish(letibot_sessionlog::SessionEvent::TranscriptAppended {
                    item_id: item_id.clone(),
                    kind: letibot_tokencore::store::item_kind(item).to_string(),
                    ledger_head: hex32(h_k),
                });
            self.hub.record_item(item_id, item.clone());
        }
        // **THE WALK ENDING IS THE OPERATION ENDING, SO SAY SO.**
        //
        // The operator watched this bar freeze and then vanish: *"it was back and kinda frozen at
        // 44 — the bar wasn't moving for seconds and then disappeared."* Both halves have a cause,
        // and only one of them is the head's.
        //
        // `continue` above skips a ledger row with no matching item, so when the two disagree the
        // walk ends on an index BELOW `total` and the last tick anybody receives is short of it.
        // The head then holds a bar that says `44 of N` with no completing tick to clear it —
        // `note-filling` clears on `done == total` and on nothing else — until its 15-second
        // no-news window expires and the line disappears. That expiry is a good guard against a
        // daemon that has DIED and a bad way to end an operation that has finished: the bar
        // vanishes because a timer ran out, not because the work completed.
        //
        // So the completion is published unconditionally, after the walk. It costs one event and
        // it makes the bar end by FACT rather than by silence — which is the rule the head's own
        // docstring states (*"a bar that cannot end is worse than no bar"*) and the reason
        // `done == total` is the clear.
        //
        // This is the second time this exact shape has been measured; leticl's `filling-active-p`
        // records the first (`57 of 1790 rows` for three and a half minutes). A head-side timer
        // hid it that time, which is why it came back.
        self.filling(what, "rows", total, total);
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

    /// The project root this session's tools are confined to — the session's own,
    /// resolved from the store at open, not necessarily the directory the daemon
    /// started in. The `/mode` command keys the per-project store on it.
    pub fn workspace(&self) -> &std::path::Path {
        &self.cfg.workspace
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
        self.runtime.gate.supervising()
    }

    /// Point this session's guard at `endpoint`, build it and attach it.
    ///
    /// For `/supervise HOST:PORT`, which is the override and not the normal path —
    /// the normal path is `[gatekeeper] endpoint` in the operator's config, read at
    /// daemon start. Nothing is written here: config is the operator's file to edit,
    /// and a harness that rewrote it behind them would make a one-off override
    /// permanent without being asked.

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
                "**no result — the executor is gone.**\n\n{}                 \n\nThe call was dispatched and nothing ever answered it. The daemon's tool thread \
                 and the call's process were both gone by the time this was noticed, and the round \
                 had already ended, so there was nothing left to interrupt.\n\n{}",
                format!("`{name}` (call `{call_id}`)"),
                tail,
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
            if let Some(hub) = self.hub.clone().into() {
                let _ = hub;
            }
            settled += 1;
        }
        if settled > 0 {
            let _ = self.append_results(results);
        }
        settled
    }

    /// The last `n` rows of this session's STORED transcript, as compact text.
    ///
    /// The durable record rather than the in-memory one: the store is what survives a restart, and
    /// it is where a reader would go looking afterwards. `None` when there is no store or no
    /// transcript — said as itself rather than as an empty tail, because "I could not look" and
    /// "there was nothing there" are different facts (`DISCLOSURE`).
    fn stored_tail(&self, n: usize) -> String {
        let Some(store) = self.store.as_ref() else {
            return "The store is not open, so the logs could not be read.".into();
        };
        let loaded = match store.load_transcript(&self.transcript_id) {
            Ok(l) => l,
            Err(e) => return format!("The logs could not be read: {e}"),
        };
        let items = &loaded.items;
        let start = items.len().saturating_sub(n);
        let mut out = String::from("the last rows of this session's own log:\n");
        for (item, _, _) in &items[start..] {
            let line = match item {
                TranscriptItem::User { parts, .. } => {
                    let text: String = parts
                        .iter()
                        .map(|p| match p {
                            UserPart::Text { text } => text.clone(),
                            _ => "[non-text part]".into(),
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("  user: {}", one_line(&text, 90))
                }
                TranscriptItem::Assistant {
                    text, tool_calls, ..
                } => {
                    if text.trim().is_empty() && !tool_calls.is_empty() {
                        format!(
                            "  assistant: (no prose) -> {} call(s): {}",
                            tool_calls.len(),
                            tool_calls
                                .iter()
                                .map(|c| c.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else {
                        format!("  assistant: {}", one_line(text, 90))
                    }
                }
                TranscriptItem::ToolResult {
                    name,
                    outcome,
                    payload,
                    ..
                } => format!("  {name} -> {outcome:?}: {}", one_line(payload, 70)),
                TranscriptItem::System { text, .. } => format!("  system: {}", one_line(text, 80)),
                // **The two that carry no facts about the RUN.** Reasoning is the model thinking
                // and a segment mark is the head's own boundary between runs; a log tail looking
                // for *what happened to this call* names neither, and inventing a rendering for
                // them here would put two rows of the model's thoughts into a sentence about a
                // missing result.
                TranscriptItem::Reasoning { .. } | TranscriptItem::SegmentMark { .. } => continue,
            };
            out.push_str(&line);
            out.push('\n');
        }
        out
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
        self.runtime
            .gate
            .attach_advisor(std::sync::Arc::from(advisor))?;
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
        let dropped = self.runtime.gate.set_mode(mode)?;
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
        if mode.decider == letibot_tools::mode::Decider::Model && !self.runtime.gate.supervising() {
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
        self.wiring.adjudicator = self.runtime.gate.describe();
        self.publish_settings();
        Ok(said)
    }

    /// Push the settings this session runs under to the registry, where the
    /// server answers a head's `Settings` from. On open and on every runtime
    /// change, so the pane never shows a mode that was true a minute ago.
    fn publish_settings(&self) {
        self.session_registry.set_settings(
            &self.cfg.session_id,
            self.cfg.settings(
                &self.mode_source,
                self.runtime.gate.supervising(),
                &self.door_tools(),
            ),
        );
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
        if on && !self.runtime.gate.supervising() {
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
        let said = self.runtime.gate.set_supervision(on)?;
        // The banner is read after this, and a disclosure that still said
        // `--adjudicator head` about a supervised session would be the constant-banner
        // defect `GateWiring` exists to prevent.
        self.wiring.adjudicator = self.runtime.gate.describe();
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

    /// The session's todo list as the model last wrote it — the board's
    /// snapshot, which is what a resume restored and what `flush_todos` keeps
    /// durable.
    pub fn todo_list(&self) -> Vec<TodoItem> {
        self.todos.snapshot()
    }

    pub fn ledger_head(&self) -> String {
        self.session.ledger_head()
    }

    /// The ledger's whole token count — prefix and body together — which is the
    /// number a compaction is measured against.
    pub fn ledger_len(&self) -> usize {
        self.session.ledger.len()
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
        self.trail.begin_turn();
        self.trail
            .say(Speaker::Operator, text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            // **The one door the operator's own words come through**, and it says so on the
            // row — the same speaker the trail records one line up (R42).
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text { text: text.into() }],
        })
    }

    /// This session's monitors, or `None` on a backend that cannot start processes.
    ///
    /// The daemon needs them to arm the wake; nothing else does.
    pub fn monitors(&self) -> Option<&Arc<Monitors>> {
        self.monitors.as_ref()
    }

    /// **Switch to a model this fleet hosts** — `/models dense78`.
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
    /// happen.
    ///
    /// Three outcomes, and each is a sentence rather than a shrug:
    ///
    ///   * `/props` names the same GGUF this daemon tokenizes with — switch.
    ///   * `/props` names a different one — refuse, and print both names.
    ///   * nothing answers, or `/props` names no model at all (a proxy, a server
    ///     without it) — refuse, and name the block key that asserts it by hand.
    ///     An operator saying *"I know these are the same weights"* is a decision on
    ///     the record; this guessing it would not be.
    pub fn set_local_model(
        &mut self,
        m: &letibot_provider::keys::LocalModel,
    ) -> Result<String, HarnessError> {
        let want = parse_local_url(&m.url)
            .map_err(|e| HarnessError::Setup(format!("[model.\"{}\"] {e}", m.name)))?;
        if let Some(why) = local_model_vocab_refusal(&want, m, &self.cfg.vocab_gguf) {
            return Err(HarnessError::Setup(why));
        }

        // Off any metered provider first, and by its own door, so the ledger scale and
        // the restored window are handled where that is understood. Its report is
        // discarded deliberately: it describes a move to the daemon's own server, which
        // is a waypoint here rather than where this switch lands.
        self.set_provider(None)?;

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
        let moved = self.retune_window_local(&want, &m.name, m.profile.window);
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

    /// **Switch what answers this session's turns**, underneath the conversation.
    /// `None` is the local server. The transcript, the ledger and the tools are
    /// untouched: the next turn simply goes elsewhere. Returns a line saying what
    /// answers now; a provider whose key cannot be found is refused and nothing
    /// changes.
    pub fn set_provider(
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
        // **The clear is inside each arm and not before the `match`**, because this method
        // promises that a switch which cannot be built changes nothing — and a scale dropped
        // for a provider whose key turned out to be missing would be a change made by a
        // refusal.
        match choice {
            None => {
                self.provider = None;
                self.cfg.ledger_scale = None;
                self.cfg.provider = None;
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
                self.publish_settings();
                Ok(line)
            }
            Some(pc) => {
                let p = build_provider(&pc, &self.cfg.sampling).map_err(HarnessError::Setup)?;
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
            .map(|j| letibot_sessionlog::protocol::JobEntry {
                id: j.id.0.clone(),
                command: Self::one_line(&j.command, 120),
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
            })
            .collect()
    }

    /// Push the job table to the registry, where a head's `ListJobs` is answered
    /// from. Called wherever the table can have changed, for the same reason
    /// `publish_settings` is: a mailbox nobody refills is a stale answer that
    /// looks like a current one.
    pub fn publish_jobs(&self) {
        self.session_registry
            .set_jobs(&self.cfg.session_id, self.job_entries());
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
            out.push(format!(
                "  {}  {}  {} bytes  {}",
                j.id.0,
                j.state.word(),
                j.produced,
                Self::one_line(&j.command, 60)
            ));
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

    /// **A prompt has arrived: this is where the whole turn's clock starts.**
    ///
    /// Called once per prompt, never per round — the engine's `turn_began_ms` is then carried on
    /// every `TurnStarted` that prompt produces, which is what stops a head restarting its clock
    /// at each round. The engine field is private to this module and this is the only writer, so
    /// the two cannot drift out of step.
    pub fn begin_turn_clock(&mut self, began_ms: u64) {
        self.turn_began_ms = Some(began_ms);
        self.engine.turn_began_ms = Some(began_ms);
    }

    /// The turn is over: stop publishing a start, so a later round that belongs to a NEW prompt
    /// cannot inherit this one's clock.
    pub fn end_turn_clock(&mut self) {
        self.turn_began_ms = None;
        self.engine.turn_began_ms = None;
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
    /// **The version bump IS the announcement.** `set_operator` raises the board's version, and
    /// `flush_todos` — which runs at every turn boundary — publishes a `TodosUpdated` and persists
    /// when the version has moved. So the operator's rows reach every head and the store by the same
    /// path the model's do, with no second mechanism.
    pub fn set_operator_todos(&mut self, items: Vec<letibot_tokencore::store::TodoItem>) {
        self.todos.set_operator(items);
        // **AND PERSIST IT AND TELL EVERY HEAD, which is the half the version bump alone does not
        // do.** `flush_todos` is version-gated and runs at every TURN BOUNDARY — so on a session
        // where no turn ever runs again, the operator's rows would sit on the board unpublished and
        // unwritten, and `Sessions::dispatch` is the one place that can flush them at the moment they
        // arrive. MEASURED as an omission by reading the path rather than by watching it fail: the
        // bump makes `flush_todos` willing, and nothing made it happen.
        //
        // The row does not reach the model's TOOL REPLY — that is `todo`'s own — so nothing here is
        // a turn.
        if let Err(e) = self.flush_todos() {
            eprintln!("  todos: could not publish the operator's row: {e}");
        }
    }

    pub fn nag_notice(&self) -> Option<String> {
        unfinished_plan(&self.todos.snapshot())
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
        let Some(text) = self.nag_notice() else {
            return Ok(None);
        };
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
        .map(Some)
    }

    /// **Stop this session's children, and say so on its own log** — the downward edge, for a
    /// caller that holds a `Harness` rather than a `HubSteering`.
    ///
    /// Two callers, and they are two of the three doors a stop arrives through: [`serve_child`]'s
    /// stop arm (a child between turns, on the thread that runs it) and `Sessions::dispatch`'s
    /// interrupt arm (a session the daemon holds, with nothing generating). The third — the
    /// steering source's interrupt arm, for a turn in flight — holds no `Harness` and calls
    /// [`stop_children_first`] directly, which is the one implementation all three go through.
    pub fn stop_children(&self) -> Option<String> {
        stop_children_first(Some(&self.subagents), &self.hub)
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
        // **Two kinds of thing arrive between turns, and they share one turn.**
        //
        // A monitor that fired, and a background job that ended. Both are the machine's
        // reading of the world with nobody having asked, both must reach the model
        // unprompted, and neither may jump a person who is waiting: that ordering is
        // `Bell::next_any`'s — commands drain before wakes — and it holds for both
        // because both arrive as `Work::Woken`.
        let mut notices: Vec<String> = Vec::new();
        if let Some(monitors) = self.monitors.clone() {
            let since = self.monitor_cursor.load(Ordering::SeqCst);
            let settled = monitors.settled_count();
            if settled > since {
                let fired: Vec<_> = monitors.firings().into_iter().skip(since).collect();
                self.monitor_cursor.store(settled, Ordering::SeqCst);
                if !fired.is_empty() {
                    notices.push(monitor_notice(&fired));
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
        let done = self
            .job_watch
            .as_ref()
            .map(|w| w.take_completions())
            .unwrap_or_default();
        // The sentences, and which kind gets which — [`completion_notices`], which is where that
        // decision lives so that it can be asserted without a live session.
        notices.extend(completion_notices(done, &self.hub.session_id()));
        // **And the rows whose condition just became met.** The same door, because a condition
        // firing is the same kind of fact as a job ending: nothing asked, and the model has to be
        // told. What the line carries is BOTH halves — the row's own words and what the world says
        // about its handle — because either alone is unusable: an intent with no fate is a sentence
        // the model cannot act on, and a fate with no intent is the settlement notice it already
        // has.
        let due = self.due_todo_rows();
        let fired: Vec<String> = due.iter().map(|(_, handle, _)| handle.clone()).collect();
        if !due.is_empty() {
            let lines: Vec<String> = due
                .iter()
                .map(|(text, handle, fate)| format!("  - {text} — {}", fate_line(handle, fate)))
                .collect();
            notices.push(format!(
                "[todo] {} row you attached a condition to is due:\n{}\nThe condition is met and \
                 the row itself is unchanged — it is yours to act on.",
                due.len(),
                lines.join("\n")
            ));
        }
        if notices.is_empty() {
            return Ok(None);
        }
        let text = notices.join("\n\n");
        // The harness talking to itself, not the operator. A firing — or a job's end —
        // must never be able to authorise the action it reports on.
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            // The same speaker the trail records, one line up: this is the session talking
            // to itself, and a head must not draw it as the operator's words (R42).
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
        .map(Some)
        // ...and the condition is spent only once the model HAS been told. Consuming before the
        // submit would let a turn that failed to start silently eat the firing, and **a firing
        // nobody hears is the failure this whole mechanism exists to rule out**, where a firing
        // reported twice is merely dull. So the order here is the requirement and not tidiness.
        .map(|out| {
            if !fired.is_empty() {
                self.todos.consume_conditions(&fired);
                if let Err(e) = self.flush_todos() {
                    eprintln!("  todos: a fired condition could not be written down: {e}");
                }
            }
            out
        })
    }

    /// **The turn after the wall, without a human typing "continue".**
    ///
    /// A wall stop ends the turn with the work half-done, and the compaction that
    /// follows it (`Sessions::after_turn`) replaces the transcript with a summary —
    /// after which, until now, nothing ran. The worker went idle, the prompt was
    /// still open, and the operator typed "continue" by hand to start the turn the
    /// summary had just recorded the state of. Measured on 2026-09-15 and on every
    /// compaction since: *"again have to write it after compaction"*.
    ///
    /// opencode never had the hole, because its compaction is a step inside the run
    /// loop and the run never ends. This harness cannot run compaction — it reaches
    /// the store, the resume chain and the registry, which `Sessions` owns — so the
    /// seam is a user item in the harness's own voice, the same shape as
    /// [`Harness::wake`]: `Speaker::Agent`, never an authorisation, and never
    /// readable as the operator's words. `Sessions::after_turn` calls this while
    /// there is room and a bound of continuations left; a wall the compaction did
    /// not clear still comes back as [`HarnessError::ContextWall`], which is the
    /// honest answer.
    pub fn continue_after_wall(&mut self) -> Result<Reply, HarnessError> {
        let text = "Your previous turn was stopped at the context wall and the conversation \
                    was then compacted: the summary above stands in for everything said \
                    before it. Continue the work you were doing, picking up from the \
                    summary rather than repeating what it already records.";
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text: text.into() }],
        })
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
    ///   [`WALL_CONTINUES`](crate::sessions::WALL_CONTINUES) exactly as
    ///   `after_turn`'s loop is;
    /// * every announcement is the same line the daemon's session gets, named with
    ///   this session's id — the log contrast that settled the finding (a parent
    ///   with `compacting:` lines beside a child with none) is evidence this must
    ///   never leave unreadable again.
    ///
    /// This is NOT a second compaction policy: the predicate, the bound and the
    /// wording are the same ones `Sessions` uses, reached from the side that owns
    /// the harness. `Sessions::after_turn` keeps its own copy of the tail because it
    /// owns obligations a child's thread has no part of — publishing the turn's
    /// failure first, the todo nag, the turn clock — and collapsing the two now
    /// would rewire the daemon's own path for a fix that is not about it.
    pub fn submit_as_a_normal_session(&mut self, prompt: &str) -> Result<Reply, HarnessError> {
        // Before the send, the same pre-turn check `Sessions::run_prompt` makes: a
        // parked child taking a later prompt is the resumed-session case (its last
        // turn may have filled the window with nothing behind it to check), and a
        // fresh child's bare prefix fails `should_compact` and costs one comparison.
        self.compact_if_at_the_wall();
        let mut out = self.submit(prompt);
        // **A wall that compacted is not the end of the prompt** — `after_turn`'s
        // own rule, and the loop shape is deliberately the same: compact, MEASURE
        // the room (never read it off `auto_compact` — the no-progress guard turns
        // the flag off without freeing anything), continue, and let a continuation
        // that meets the wall again come back around for another compaction until a
        // gate closes. When one does, the wall error from the last turn is what the
        // caller gets, which is the honest answer for a task that does not fit the
        // window.
        let mut walls = 0usize;
        while matches!(out, Err(HarnessError::ContextWall { .. })) {
            let attempted = self.compact_if_at_the_wall();
            if !attempted {
                // The same contradiction `after_turn` reports: a wall fired and
                // nothing compacted, so WHICH number disagreed is said rather than
                // left to be inferred from a silence the wall notice contradicts.
                self.hub.publish(SessionEvent::Warning {
                    code: "auto_compact_skipped".into(),
                    detail: format!(
                        "the turn stopped at the context wall and nothing was compacted: \
                         automatic compaction is {} for this session. `/compact` does it \
                         by hand.",
                        if self.cfg.context_window.is_none() {
                            "not configured — no --context-window is set, so there is no \
                             wall to measure against"
                        } else {
                            "off"
                        }
                    ),
                    compaction: None,
                });
                break;
            }
            if !self
                .cfg
                .room_for_next_turn(self.session.ledger.len() as u64)
            {
                break;
            }
            walls += 1;
            if walls > crate::sessions::WALL_CONTINUES {
                break;
            }
            out = self.continue_after_wall();
        }
        out
    }

    /// `Sessions::compact_if_at_the_wall`'s contract, from the side that owns the
    /// harness: **compact when the next turn would not fit**, announce it in the
    /// same words the daemon's own sessions get, and apply the no-progress guard
    /// that stops a too-large summary from looping once per turn.
    ///
    /// Same name as the `Sessions` method on purpose: it is the same seam, and the
    /// next person asking *why does this resurface* should find both doors in one
    /// grep. The sibling's doc carries the history (the measured 2026-09-15 wall
    /// that created it); this one carries the child's half.
    ///
    /// Returns whether a compaction was actually ATTEMPTED — `false` when the
    /// session is not at the threshold, or automatic compaction is off, or no
    /// window is configured. A wall notice that says *"a compaction was
    /// ATTEMPTED"* needs that to be a fact rather than a hope, and until this
    /// existed a child's was a promise nothing behind it kept.
    fn compact_if_at_the_wall(&mut self) -> bool {
        let resident = self.session.ledger.len() as u64;
        if !self.cfg.should_compact(resident) {
            return false;
        }
        // **Shown in the operator's units, decided in the ledger's** — the same rule
        // the `Sessions` sibling states: `should_compact` above is the decision,
        // `shown_tokens` is only the telling.
        let scale = self.cfg.ledger_scale;
        let (r, w, h) = (
            self.cfg.shown_tokens(resident),
            self.cfg
                .shown_tokens(self.cfg.planning_window().unwrap_or(0)),
            self.cfg.shown_tokens(self.cfg.headroom()),
        );
        // **And it names the session, for the reason the sibling's line does**: the
        // log is one file every daemon appends to, and a child's compaction lines
        // land between its parent's work with nothing to say whose they were.
        eprintln!(
            "  {}: compacting: {r} of {w} tokens resident, less than the {h} the next \
             turn needs",
            self.cfg.session_id
        );
        self.hub.publish(SessionEvent::Warning {
            code: "auto_compact".into(),
            detail: format!(
                "{r} of {w} tokens resident, leaving less than the {h} the next turn \
                 needs — compacting now, as one more message so the prefix the server \
                 already holds is reused. This is the wall, not a judgement about the \
                 conversation."
            ),
            compaction: None,
        });
        match self.compact() {
            Ok(report) => {
                self.hub.publish(SessionEvent::Warning {
                    code: "compacted".into(),
                    detail: crate::sessions::compaction_said(&report, scale),
                    compaction: Some(Box::new(crate::sessions::compaction_wire(
                        &report,
                        "compacted",
                        scale,
                        r,
                        w,
                        h,
                    ))),
                });
                let after = self.session.ledger.len() as u64;
                // **If it did not help, stop trying** — the sibling's guard, for the
                // sibling's reason: a summary that is itself over the threshold
                // would compact again next turn, and again, a loop that spends a
                // turn each time and never lets the conversation continue. Turning
                // `auto_compact` off also stands down the round-loop wall check for
                // this session, which is the same trade the daemon's sessions make.
                if self.cfg.should_compact(after) {
                    self.cfg.auto_compact = false;
                    self.hub.publish(SessionEvent::Warning {
                        code: "auto_compact_no_progress".into(),
                        detail: format!(
                            "compacted from {r} to {} tokens and that is STILL within \
                             {h} of the {w} window, so automatic compaction is now off \
                             for this session rather than looping once per turn. The \
                             summary itself is near the wall: start a fresh session, or \
                             raise --context-window if the server really has more.",
                            self.cfg.shown_tokens(after)
                        ),
                        compaction: None,
                    });
                } else {
                    let cut = if report.fork.truncated {
                        " The summary was CUT OFF at the model's length limit — it is \
                         incomplete, and the base says so too."
                    } else {
                        ""
                    };
                    let after = self.cfg.shown_tokens(after);
                    eprintln!(
                        "  {}: compacted: {after} tokens resident now, was {r}.{cut}",
                        self.cfg.session_id
                    );
                    self.hub.publish(SessionEvent::Warning {
                        code: "auto_compact".into(),
                        detail: format!("compacted: {after} tokens resident now, was {r}.{cut}"),
                        compaction: None,
                    });
                }
            }
            Err(e) => {
                // Announced and swallowed, for the same reason as the sibling's: the
                // room check in `submit_as_a_normal_session` is what decides the
                // turn's fate, and a compaction that did not run leaves no room —
                // so the wall error stands, and this line is why the operator can
                // see it was tried rather than infer it never was.
                eprintln!("  {}: compaction FAILED: {e}", self.cfg.session_id);
                self.hub.publish(SessionEvent::Warning {
                    code: "auto_compact_failed".into(),
                    detail: format!(
                        "the automatic compaction did not run: {e}. This session is over \
                         its context budget and nothing made room, so the next turn will \
                         hit the context wall. `/compact` retries it."
                    ),
                    compaction: None,
                });
            }
        }
        // It ran. Whether it HELPED is the branch above, in its own words either
        // way; what this answers is the narrower question a caller asks before
        // claiming an attempt was made.
        true
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

    pub fn compact(&mut self) -> Result<CompactReport, HarnessError> {
        self.compacting = true;
        let out = self.compact_inner();
        self.compacting = false;
        out
    }

    /// **Re-seat this conversation onto the tool list that is seated NOW.**
    ///
    /// The tool schemas live in the stable prefix, and a session's prefix is fixed
    /// the moment it is created — a resume replays the stored one because the
    /// stored tokens were produced under it, and a compaction fork keeps it for the
    /// same reason. So a conversation started by a daemon with no shell can never
    /// call one, however the daemon that reopened it is seated: the registry has
    /// the tool, the prompt has never heard of it, and the banner — computed from
    /// the registry — says `exec` while the model sits there unable to.
    ///
    /// That is the whole of the operator's hour: *"i did letibot --stop and
    /// leticode --continue but still no exec"*, and then the diagnosis, which was
    /// theirs: *"i guess it is because we announce tools once at the start of the
    /// session"*. Exactly so.
    ///
    /// What cannot be done is swapping the list under a live transcript: every
    /// token in it was produced under the old prefix, and appending to them under a
    /// new one builds a prompt no model was trained on — the same rule the dialect
    /// check enforces at resume. What CAN be done is what compaction already does
    /// for a different reason: summarise, then continue in a fresh transcript. The
    /// only difference here is which prefix the fork opens under, so this is
    /// `compact` with one argument changed.
    ///
    /// The cost is stated because it is real: the conversation continues from a
    /// summary, not from its own tokens, and the prefix is new so the server's
    /// cache for it is cold. Both are what changing the announced tools costs, and
    /// a version that hid either would be hiding the thing the operator is paying.
    /// **The prompt this daemon would seat a NEW conversation with**, and the
    /// store row for it — or `None` when that is already the prompt being spoken.
    ///
    /// Both `/reseat` and every compaction fork want the same two values, computed
    /// the same way, so they are computed once here. The probe render is what
    /// makes the row honest: a `stable_prefix` written with an empty token list
    /// verified as `hash chain broken at row 0` on the way back in, because the
    /// tokens are what a resume rebuilds the chain from.
    /// Public for the same reason [`Harness::fork_to_summary`] is: the summary
    /// turn needs a model server and this does not, so the re-seat half of a
    /// compaction is drivable — and therefore testable — offline.
    pub fn reseat_target(&mut self) -> Result<Option<(StablePrefix, String)>, HarnessError> {
        let next = StablePrefix {
            system: self.cfg.system.clone(),
            tools_json: self.render.tools_json(&self.runtime.registry.schemas()),
        };
        if next == self.prefix {
            return Ok(None);
        }
        // No store is not a refusal here — a compaction without one refuses later
        // and for its own reason. It is simply nowhere to record a new prefix, so
        // the fork keeps the old one.
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        let _ = store;
        let measured = self
            .engine
            .open(&format!("{}#reseat-probe", self.cfg.session_id), &next)
            .map_err(|e| HarnessError::Setup(format!("rendering the new prompt: {e}")))?;
        let rec = StablePrefixRecord {
            dialect_sha: hex32(&self.render.spec().template_sha),
            system: next.system.clone(),
            tools_json: next.tools_json.clone(),
            tokens: measured.ledger.prefix_tokens().to_vec(),
            h_init: measured.ledger.h_init(),
            vocab_source: self.cfg.vocab_gguf.display().to_string(),
        };
        drop(measured);
        let id = self
            .store
            .as_ref()
            .expect("checked above")
            .put_stable_prefix(&rec)
            .map_err(|e| HarnessError::Store(e.to_string()))?;
        Ok(Some((next, id)))
    }

    /// **Re-seat without summarising: carry the whole conversation across.**
    ///
    /// **This is what `/reseat` does.** The operator asked for it — *"is there a
    /// way to reseat without summarizing? … like reingest full context"* — and
    /// then made it the default: *"id say flip it - reset is loseless and reset
    /// summarize will be not"*. A verb named for message zero should not cost
    /// you everything under message zero unless you said so.
    ///
    /// A tool's schema lives in the stable prefix, which is message zero, so a
    /// running session keeps the prompt it opened under and a schema change
    /// needs a fork. [`Harness::reseat`] pays for that fork with a summary turn
    /// — which is right when the point is to shrink, and pure loss when the
    /// point is only to change message zero.
    ///
    /// This forks onto the seated prompt with every item carried verbatim. The
    /// machinery is `fork_to_summary`'s own: `tail` has always been "items kept
    /// as themselves", and this passes all of them with no summary in front.
    ///
    /// **What it costs.** The prefix changed, so nothing the server has cached
    /// matches and the next turn pays a cold prefill of the WHOLE history — on a
    /// metered provider, the whole conversation re-sent and re-billed once. That
    /// is the trade against `reseat`, and it is the operator's to make: a summary
    /// is cheaper and lossy, this is dearer and lossless. The default is the
    /// lossless one, because a re-seat that quietly ate the conversation is a
    /// surprise you cannot undo, and a bill is one you can see coming.
    ///
    /// It refuses where it cannot help: over the window, a verbatim carry would
    /// fork onto a base that cannot be prefilled at all, and the honest answer
    /// there is the summary it was trying to avoid.
    pub fn reingest(&mut self) -> Result<ReseatReport, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "re-seating forks the conversation onto a new prompt, and a fork needs a \
                 store to write it to; this session has none."
                    .into(),
            ));
        }
        let Some((next, next_id)) = self.reseat_target()? else {
            return Err(HarnessError::Setup(
                "this conversation's prompt already carries exactly the tools that are \
                 seated, so there is nothing to re-seat."
                    .into(),
            ));
        };
        // **The carry has to fit, and "fit" has to be measured in ONE unit.**
        //
        // `resident` below is counted off the ledger, with this box's own
        // vocabulary. On a metered provider that is not the number the provider
        // will refuse at and not the number the operator's header shows: the
        // ledger holds the reasoning rows that `letibot_provider::messages` drops,
        // so it over-counts. Measured on the operator's own session, 2026-09-20:
        // 991,596 in the ledger against 671,280 the provider counted for the same
        // conversation, 49% apart.
        //
        // This compared the ledger figure against `planning_window()`, which is
        // the window in LEDGER units — correct, but only while `ledger_scale` is
        // known. Unmeasured, `planning_window` hands back the provider's own
        // window unchanged, and the comparison silently became ledger-against-
        // provider. That refused a `/reseat` whose carry had 400k tokens of room,
        // and printed `991596 of 1000000` under a header reading 671k with no way
        // to reconcile the two.
        //
        // So: convert to the PROVIDER's units and compare there, because that is
        // the number the operator can check. And when nobody has measured the
        // ratio yet, **do not refuse** — an unmeasured ratio is not a small one,
        // the error is all in one direction, and losing a lossless re-seat over a
        // conversion the daemon admits it cannot do is the worse failure.
        let per_item: Vec<u64> = (0..self.session.items.len())
            .map(|i| {
                self.session
                    .ledger
                    .item_tokens(i)
                    .map(|t| t.len() as u64)
                    .unwrap_or(0)
            })
            .collect();
        let resident: u64 = per_item.iter().sum();
        // In the provider's units where they differ, and the ledger's where they
        // do not — a local endpoint counts with the vocabulary the ledger uses, so
        // `provider_tokens` is `None` there and the ledger figure IS the figure.
        let carried = match self.provider.is_some() {
            true => self.cfg.provider_tokens(resident),
            false => Some(resident),
        };
        if let Some(window) = self.cfg.context_window
            && let Some(carried) = carried
            && carried + self.cfg.headroom() >= window
        {
            let note = if carried == resident {
                String::new()
            } else {
                format!(
                    " (that is {resident} token(s) in this box's own ledger; the provider \
                     counts the same conversation as {carried}, and its number is the one \
                     that decides)"
                )
            };
            return Err(HarnessError::Setup(format!(
                "carrying this conversation verbatim would put {carried} token(s) in front \
                 of a {window}-token window{note}, leaving less than the {} a turn needs. \
                 Re-seating without summarising only works while the conversation still \
                 fits; `/reseat summarise` summarises and fits, and `/compact` does the \
                 same without changing the prompt. Nothing was changed.",
                self.cfg.headroom()
            )));
        }
        // Unmeasured ratio: carry it, and say that the fit was not checked rather
        // than pretend either way. The first metered turn measures the scale, so
        // this is a window of one turn per daemon and not a standing hole.
        if carried.is_none() {
            self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                code: "reseat_unchecked".into(),
                detail: format!(
                    "this session has not taken a metered turn yet, so how the provider \
                     counts it is unmeasured and the carry could not be checked against \
                     the window. Going ahead: {resident} token(s) by this box's ledger, \
                     which over-counts what a provider is sent. If the next turn does not \
                     fit, `/compact` is the way back."
                ),

                compaction: None,
            });
        }

        let before = tool_names(&self.prefix.tools_json);
        let after = tool_names(&next.tools_json);
        let items: Vec<TranscriptItem> = self.session.items.clone();

        // No summary turn at all — that is the whole point — so the outcome is
        // the empty one, and `fork_to_summary` reads an empty summary as "this
        // was not a compaction" and writes the note that says so.
        let outcome = CompactionOutcome {
            turn_id: format!("{}#reingest", self.transcript_id),
            summary: String::new(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
        };
        self.compacting = true;
        let out = (|| -> Result<ReseatReport, HarnessError> {
            // **The whole conversation, carried as it is** — that is what a re-ingest is,
            // and an empty `because` says no decision about a tail was taken. It goes
            // through `ForkTail` rather than a bare slice because the fork needs one
            // shape for both callers, not because this is a compaction.
            let fork = self.fork_to_summary(
                &outcome,
                Some(&next),
                Some(&next_id),
                ForkTail {
                    items: &items,
                    split: None,
                    because: "",
                },
            )?;
            Ok(ReseatReport {
                fork,
                summary_turn: outcome,
                gained: after.difference(&before).cloned().collect(),
                lost: before.difference(&after).cloned().collect(),
            })
        })();
        self.compacting = false;
        let mut out = out?;
        out.gained.sort();
        out.lost.sort();
        self.publish_settings();
        Ok(out)
    }

    /// **Re-seat and summarise**, replacing the conversation with the summary.
    ///
    /// This is `/reseat summarise`: the lossy kind, and it is the one you ask for
    /// by name. A bare `/reseat` runs [`Harness::reingest`]. Reach for this when
    /// the point is to shrink as well as to change message zero, or when the
    /// conversation no longer fits in front of the window.
    pub fn reseat(&mut self) -> Result<ReseatReport, HarnessError> {
        if self.store.is_none() {
            return Err(HarnessError::Setup(
                "re-seating forks the conversation onto a new prompt, and a fork needs a \
                 store to write it to; this session has none."
                    .into(),
            ));
        }
        // The same two values a compaction computes, computed the same way. A
        // `None` here is the conversation already speaking the seated prompt.
        let Some((next, next_id)) = self.reseat_target()? else {
            return Err(HarnessError::Setup(
                "this conversation's prompt already carries exactly the tools that are \
                 seated, so there is nothing to re-seat. Nothing was changed, and nothing \
                 needed to be: every compaction now forks onto the seated prompt, so a \
                 conversation that has compacted since the daemon started is already on it."
                    .into(),
            ));
        };

        let before = tool_names(&self.prefix.tools_json);
        let after = tool_names(&next.tools_json);

        self.compacting = true;
        let out = (|| -> Result<ReseatReport, HarnessError> {
            let mut sink = CapturingSink::new(self.hub.clone());
            let answerer = Self::answerer(&self.provider, &self.prefix);
            let outcome = Self::run_summary_turn(
                &self.hub,
                &mut self.engine,
                &mut self.session,
                &mut sink,
                &answerer,
            )?;
            if outcome.tool_calls > 0 {
                return Err(HarnessError::Setup(format!(
                    "the summary turn proposed {} tool call(s); a summary is a record, not                      an action, so nothing was re-seated.",
                    outcome.tool_calls
                )));
            }
            let fork =
                self.fork_to_summary(&outcome, Some(&next), Some(&next_id), ForkTail::NONE)?;
            // Only after the fork has landed: until then this harness is still
            // speaking the old prompt, and a `self.prefix` that ran ahead of the
            // transcript would make every later turn build the wrong bytes.
            self.prefix = next;
            self.prefix_id = next_id;
            Ok(ReseatReport {
                fork,
                summary_turn: outcome,
                gained: after.difference(&before).cloned().collect(),
                lost: before.difference(&after).cloned().collect(),
            })
        })();
        self.compacting = false;
        out
    }

    /// **Compaction is one summary turn appended to the conversation itself** --
    /// unless the conversation arrived with no room to hold one.
    ///
    /// The ordinary path is the cheap one and is used whenever it can be: the
    /// server already holds the prompt, so the prefill is one message. The
    /// operator's rule, after a batched rewrite that used a fallback for
    /// everything and was reverted for it: *"when no overrun - normal
    /// compaction"*.
    ///
    /// OVERRUN is when it arrives over the budget -- *"either unlucky tool like a
    /// file read or model change"* -- so the summary turn has nowhere to write.
    /// Then, and only then, the conversation is summarised somewhere else, and
    /// WHICH way depends on the backend rather than on the conversation:
    ///
    ///   * a cloud provider takes messages rather than tokens, and a prefix-cache
    ///     miss is priced rather than waited on, so the clean fold is affordable:
    ///     summarise the first half, continue on `summary ++ second half`, with
    ///     the recent past kept VERBATIM.
    ///   * the local server prefills at 200-375 tok/s, where that same fold is ten
    ///     minutes of silence. So both halves are summarised instead, overlapping
    ///     at the seam, with the expensive half arranged to be a true prefix of
    ///     what the server already holds.
    ///
    /// See `crates/turn/src/compaction.rs` for the arithmetic and for why the
    /// cache and the log are two different things.
    fn compact_inner(&mut self) -> Result<CompactReport, HarnessError> {
        let mut sink = CapturingSink::new(self.hub.clone());

        // **A compaction re-seats.** It is already forking onto a fresh transcript
        // and already paying the cold prefill that a new prefix costs, so carrying
        // the tool list this daemon seats NOW is free — and not carrying it is
        // what made `/compact` then `/reseat` two summaries instead of one.
        //
        // Measured in the operator's own session (`…#t8`, five rows): row 0 is the
        // `/compact` summary; row 1 is `/reseat` asking for a summary of a
        // transcript that is already nothing but a summary; row 2 is the model,
        // with nothing to summarise, running `git log` instead — which tripped the
        // "a summary is a record, not an action" guard and refused the re-seat;
        // rows 3 and 4 are the retry. Three summary turns and two cold prefills to
        // pick up a tool list. Their reading: *"let compaction automatically
        // reseat so new tools picked up"*.
        //
        // `None` when the seated prompt is the one already being spoken, which is
        // the common case and costs a comparison.
        let reseat = self.reseat_target()?;

        let per_item: Vec<u64> = (0..self.session.ledger.rows().len())
            .map(|i| {
                self.session
                    .ledger
                    .item_tokens(i)
                    .map(|t| t.len() as u64)
                    .unwrap_or(0)
            })
            .collect();
        let prefix_tokens = self.session.ledger.prefix_len() as u64;
        // Ledger units: `per_item` and `prefix_tokens` below are the ledger's.
        let window = self.cfg.planning_window().unwrap_or(u64::MAX);

        match plan_overrun(&per_item, prefix_tokens, window) {
            // The ordinary path, and the one that runs almost always.
            OverrunPlan::NotOverrun => {
                // **The tail is planned from the history as it stands BEFORE the
                // summary turn**, and that is not a detail: `run_compaction`
                // appends its instruction and its answer to this session, and a
                // tail that could reach those would put the question and its
                // answer into the record that replaces them. The clone is the
                // same one the overrun arm takes and for the same reason.
                let items: Vec<TranscriptItem> = self.session.items.clone();
                let answerer = Self::answerer(&self.provider, &self.prefix);
                let outcome = Self::run_summary_turn(
                    &self.hub,
                    &mut self.engine,
                    &mut self.session,
                    &mut sink,
                    &answerer,
                )?;
                if outcome.tool_calls > 0 {
                    return Err(HarnessError::Setup(format!(
                        "the summary turn proposed {} tool call(s); a summary is a record, \
                         not an action, so nothing was compacted. The transcript is \
                         unchanged apart from the instruction and the turn themselves — \
                         try again.",
                        outcome.tool_calls
                    )));
                }
                // **Summary plus verbatim recent turns, on the remote path only**
                // — R27's ruled split, and `plan_compaction_tail` carries the
                // reason. A local model gets the template and nothing else,
                // because a tail here would be resident tokens competing with the
                // KV pressure that called this compaction in the first place.
                let remote = self.provider.is_some();
                let tail_plan = plan_compaction_tail(&items, &per_item, window, remote);
                let tail: Vec<TranscriptItem> = match tail_plan.from() {
                    Some(from) => items[from..].to_vec(),
                    None => Vec::new(),
                };
                let fork = self.fork_to_summary(
                    &outcome,
                    reseat.as_ref().map(|(p, _)| p),
                    reseat.as_ref().map(|(_, id)| id.as_str()),
                    ForkTail {
                        split: tail_plan.split(),
                        because: tail_because(&tail_plan, remote, !items.is_empty()),
                        items: &tail,
                    },
                )?;
                let (gained, lost) = self.adopt_reseat(reseat);
                Ok(CompactReport {
                    fork,
                    summary_turn: outcome,
                    gained,
                    lost,
                    // This arm ran `run_compaction` through the session's own
                    // sink, so every head watched the summary being written.
                    summary_was_streamed: true,
                })
            }

            OverrunPlan::Hopeless {
                prefix_tokens,
                window,
            } => Err(HarnessError::Setup(format!(
                "this session's PROMPT is {prefix_tokens} token(s) of a {window} token \
                 window, so no summary of the conversation can make room however short it \
                 is. Message zero is the system prompt and the tool schemas, and compaction \
                 never rewrites it. What helps is a larger --context-window if the server \
                 has one, a shorter --system, or fewer seated tools. Nothing was compacted."
            ))),

            plan @ OverrunPlan::Cut { .. } => {
                let items: Vec<TranscriptItem> = self.session.items.clone();
                let prefix = self.prefix.clone();
                let scratch = format!("{}#overrun", self.transcript_id);

                // A cloud provider is the affordable case; see the doc above.
                //
                // `tail_split` is carried out of the arms with the tail: the fold
                // splits halfway BY TOKENS, so it can start mid-exchange without
                // anybody having decided that, and a tail that opens with an answer
                // to a question that is no longer present is the one thing the
                // reader has to be told about (R27).
                let (harvest, tail, tail_split) = if self.provider.is_some() {
                    match plan_fold(&per_item, prefix_tokens, window) {
                        Some(split) => {
                            self.hub.publish(SessionEvent::Warning {
                                code: "auto_compact".into(),
                                detail: format!(
                                    "over budget: summarising the first {split} item(s) and \
                                     continuing on the summary plus the rest, verbatim"
                                ),

                                compaction: None,
                            });
                            let answerer = Self::answerer(&self.provider, &self.prefix);
                            let h = summarise_first_half(
                                &mut self.engine,
                                &prefix,
                                &scratch,
                                &items,
                                split,
                                &mut sink,
                                &answerer,
                            )
                            .map_err(HarnessError::Turn)?;
                            (h, items[split..].to_vec(), tail_split_of(&items, split))
                        }
                        // No workable fold: fall through to the two-half plan,
                        // which asks less of the split.
                        None => {
                            let answerer = Self::answerer(&self.provider, &self.prefix);
                            let h = summarise_overrun(
                                &mut self.engine,
                                &prefix,
                                &scratch,
                                &items,
                                &plan,
                                &mut sink,
                                &answerer,
                            )
                            .map_err(HarnessError::Turn)?;
                            (h, Vec::new(), None)
                        }
                    }
                } else {
                    let (cut, tail_from) = match plan {
                        OverrunPlan::Cut { cut, tail_from, .. } => (cut, tail_from),
                        _ => (0, 0),
                    };
                    self.hub.publish(SessionEvent::Warning {
                        code: "auto_compact".into(),
                        detail: format!(
                            "over budget with no room for a summary in place: summarising \
                             off to one side, in two halves that overlap. First the recent \
                             {} item(s), which the server has not seen and must read cold; \
                             then the older {cut} item(s), whose prompt the server already \
                             holds. This takes minutes and the context count above does not \
                             move until it lands.",
                            items.len() - tail_from
                        ),

                        compaction: None,
                    });
                    let answerer = Self::answerer(&self.provider, &self.prefix);
                    let h = summarise_overrun(
                        &mut self.engine,
                        &prefix,
                        &scratch,
                        &items,
                        &plan,
                        &mut sink,
                        &answerer,
                    )
                    .map_err(HarnessError::Turn)?;
                    (h, Vec::new(), None)
                };

                let outcome = CompactionOutcome {
                    turn_id: format!("{}#overrun", self.transcript_id),
                    summary: harvest.summary,
                    tool_calls: harvest.tool_calls,
                    truncated: harvest.truncated,
                    cached_tokens: 0,
                    reusable: 0,
                    generated_tokens: 0,
                };
                // The overrun fold's own reason, not `plan_tail`'s: this tail was placed
                // by arithmetic on the window, half way BY TOKENS, and the reason the
                // whole tail is what it is has nothing to do with a budget it was
                // measured against. Empty when there is no tail at all — a local
                // overrun summarises both halves and carries nothing.
                let because = if tail.is_empty() { "" } else { "fold" };
                let fork = self.fork_to_summary(
                    &outcome,
                    reseat.as_ref().map(|(p, _)| p),
                    reseat.as_ref().map(|(_, id)| id.as_str()),
                    ForkTail {
                        items: &tail,
                        split: tail_split,
                        because,
                    },
                )?;
                let (gained, lost) = self.adopt_reseat(reseat);
                Ok(CompactReport {
                    fork,
                    summary_turn: outcome,
                    gained,
                    lost,
                    // The scratch summary went to a `NullSink`; nobody saw it.
                    summary_was_streamed: false,
                })
            }
        }
    }

    /// Take on the prompt the fork just opened under.
    ///
    /// **Only after the fork has landed.** Until then this harness is still
    /// speaking the old prompt, and a `self.prefix` that ran ahead of the
    /// transcript would make every later turn build the wrong bytes — the same
    /// ordering `reseat` spells out at its own swap.
    /// Returns what the announced tool list gained and lost, for the report.
    pub fn adopt_reseat(
        &mut self,
        reseat: Option<(StablePrefix, String)>,
    ) -> (Vec<String>, Vec<String>) {
        let Some((prefix, id)) = reseat else {
            return (Vec::new(), Vec::new());
        };
        let before = tool_names(&self.prefix.tools_json);
        let after = tool_names(&prefix.tools_json);
        self.prefix = prefix;
        self.prefix_id = id;
        (
            after.difference(&before).cloned().collect(),
            before.difference(&after).cloned().collect(),
        )
    }

    /// **The fork.** Replace the resident history with one summary item, in a new
    /// transcript the store links back to this one.
    ///
    /// Split from [`Harness::compact`] so it can be driven without a model server:
    /// the summary turn needs one, this half needs only the vocabulary, the store
    /// and the harness — so the store, resume-chain and ledger behaviour is
    /// testable offline, and the turn side is tested in `letibot-turn`.
    ///
    /// The old transcript is persisted first and kept whole — compaction never
    /// deletes anything, it stops *carrying* it. The new transcript's body is one
    /// system-update item holding the summary; the next prompt is the prefix plus
    /// that item, which is the cold re-prefill `docs/compaction.md` §3 says is
    /// unavoidable, measured in tokens rather than minutes.
    pub fn fork_to_summary(
        &mut self,
        outcome: &CompactionOutcome,
        onto: Option<&StablePrefix>,
        onto_id: Option<&str>,
        tail: ForkTail<'_>,
    ) -> Result<ForkReport, HarnessError> {
        let tail_items: &[TranscriptItem] = tail.items;
        let tail_split = tail.split;
        let old_id = self.transcript_id.clone();
        let was_tokens = self.session.ledger.len();
        // The prefix the FORK opens under, which is the caller's choice and not
        // always this session's. A compaction keeps the one the conversation has
        // been speaking under; a re-seat is the whole point of being able to hand
        // in a different one, because the tool list lives in here.
        // Cloned rather than borrowed: `persist` below needs `&mut self`, and a
        // prefix is two strings and a small vector — cheaper than threading a
        // borrow through the whole fork.
        let onto: StablePrefix = onto.cloned().unwrap_or_else(|| self.prefix.clone());
        let onto_id: String = onto_id
            .map(str::to_string)
            .unwrap_or_else(|| self.prefix_id.clone());
        // The old transcript is flushed with the summary turn in it, and before the
        // fork writes anything: a fork that dropped the turn that justified it
        // would leave the store unable to say where the summary came from. With no
        // store this is a no-op, and the refusal below is the only thing that
        // happens.
        self.persist()?;
        let Some(store) = self.store.as_ref() else {
            return Err(HarnessError::Setup(
                "compaction needs a store to fork into; this session has none, so there \
                 is nowhere to put the replacement history and nothing to link it to"
                    .into(),
            ));
        };
        let n: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM transcript WHERE session_id = ?1",
                [self.cfg.session_id.as_str()],
                |r| r.get(0),
            )
            .map_err(|e| HarnessError::Store(e.to_string()))?;
        let new_id = format!("{}#t{}", self.cfg.session_id, n);
        // The fork point is the session **log's** seq, which is what §5.5's
        // `forked_at_seq` means: where in the session's event stream the divergence
        // happened, not where in the transcript.
        let forked_at = self.hub.head_seq();
        store
            .put_fork(
                &new_id,
                &self.cfg.session_id,
                &onto_id,
                &old_id,
                forked_at as u32,
            )
            .map_err(|e| HarnessError::Store(e.to_string()))?;
        let mut next = self
            .engine
            .open(&new_id, &onto)
            .map_err(|e| HarnessError::Setup(format!("opening the compacted transcript: {e}")))?;
        // **A record that was cut off says so, in the record.**
        //
        // This is the base every later turn reads as the whole of what came
        // before. When the summary turn ran out of room mid-sentence, that base
        // used to claim to be "the summary, written over the full history" — with
        // the last third missing and nothing anywhere saying it. A model reading
        // it cannot tell, and neither could the operator until they read to the
        // end and found `&page.final_url, &page`.
        //
        // Not a refusal: a session at the wall has nowhere else to go, and an
        // incomplete record that admits it is more useful than no compaction at
        // all. The sentence goes FIRST, before the summary, because a reader who
        // stops early is exactly the reader who needs it.
        let cut = if outcome.truncated {
            " It was CUT OFF at the model's length limit before it finished: what              follows is incomplete, it stops mid-sentence, and anything the              conversation established after the point it reaches is not in it.              Treat a gap as unknown rather than as settled, and re-read the earlier              transcript if something is missing."
        } else {
            ""
        };
        // **A fork that summarised says so; one that did not must not.**
        //
        // An empty summary is `reingest`: the prompt changed and the conversation
        // did not. Telling the model "everything before this is replaced by the
        // summary below" and then showing it no summary — with the whole history
        // underneath — is a sentence that contradicts what it can see.
        //
        // **And the sentence had to change when the tail arrived.** It said
        // "everything said before this point is replaced by the summary below",
        // which stopped being true the moment a remote compaction carried the
        // newest exchanges verbatim: the reader can see both, and a note that
        // claims a region was replaced while showing it is the same class of
        // contradiction as the re-ingest case above.
        // One implementation with the sentence the head is given: see
        // `CompactionTail::why_line`, which both readers call.
        let tail_why_text = (!tail.because.is_empty()).then(|| {
            letibot_sessionlog::event::CompactionTail {
                turns: Vec::new(),
                carried: tail_items.len() as u64,
                because: tail.because.to_string(),
                dropped: 0,
            }
            .why_line()
            .unwrap_or_default()
        });
        let carried = match tail_items.len() {
            // **WHY there is no tail, said in the row itself** — R41's shape one document
            // over: an absence with two causes and one appearance. The `carried` clause below
            // says how much when there is some; with none, this said nothing at all, so a
            // reader of the record could not tell R27's ruling working from the budget losing
            // to one item.
            //
            // **Here and not only on the wire, because this is the durable one.** The
            // `compacted` warning is published to attached heads and the store has no events
            // table: after a restart the transcript item is all a reader has, and it is where
            // the question has to be answerable. `tail.because` is empty for a re-seat, which
            // is not a compaction and gets no clause.
            0 if !tail.because.is_empty() => format!(" {}", tail_why_text.unwrap_or_default()),
            0 => String::new(),
            n => format!(
                " The last {n} item(s) of it follow this note VERBATIM — as they were \
                 written, not as a description of them."
            ),
        };
        let mid_exchange = match tail_split {
            None => String::new(),
            Some(TailSplit { dropped }) => format!(
                " The verbatim part begins in the MIDDLE of an exchange: {dropped} item(s) \
                 of it were dropped from the front, so its first message answers something \
                 that is no longer here. Stated rather than left to be discovered."
            ),
        };
        let note: TranscriptItem = TranscriptItem::System {
            text: if outcome.summary.is_empty() {
                format!(
                    "This conversation was re-seated onto a new prompt: message zero now \
                     announces the tools this daemon seats, and everything said before \
                     this point follows unchanged from transcript {old_id}. Nothing was \
                     summarised and nothing was dropped."
                )
            } else {
                format!(
                    "This conversation was compacted: what was said before this point is \
                     replaced by the summary below, which was written over the full history \
                     of transcript {old_id} and proposed no tool calls.{carried}{mid_exchange}{cut}\n\n{}",
                    outcome.summary
                )
            },
            origin: SystemOrigin::Update,
        };
        // **The recent past is carried over verbatim, not described.**
        //
        // Compaction used to keep none of it: the new base was the summary and
        // nothing else, so a session came back from a compaction unable to see the
        // turn it was in the middle of -- that turn was now a sentence about a
        // turn. The tail is the newest items, under their own budget, appended after
        // the summary so the order of the conversation is preserved: everything
        // old as prose, then the last stretch as itself.
        //
        // **Mapped to the wire's role/text as it is appended**, so the report and the
        // bytes cannot describe different tails: one pass over one slice.
        let mut body: Vec<TranscriptItem> = Vec::with_capacity(1 + tail_items.len());
        body.push(note);
        body.extend_from_slice(tail_items);
        let tail_turns = wire_turns(tail_items);

        let mut sink = CapturingSink::new(self.hub.clone());
        next.append_items(&self.engine, &body, &mut sink)?;
        // The swap is the point of no return, and it is deliberately **after** every
        // store write that could fail: a harness that swapped and then could not
        // persist would be a session whose transcript row exists but whose body does
        // not, which is the one state a resume cannot rebuild honestly.
        self.session = next;
        self.transcript_id = new_id.clone();
        self.persisted = 0;
        self.reconcile(&mut sink, &body);
        self.persist()?;
        // **The stored context size is about the OLD conversation.** It is the
        // last prompt the provider measured, and that prompt no longer exists:
        // this fork replaced the history with a summary. Only a turn can put a
        // real number here, so until one runs there is no number — the operator
        // restarted a session that had just compacted 1.5M tokens down and the
        // header still read `1.05m`, because nothing between a compaction and
        // the next turn ever wrote to this row.
        //
        // Cleared rather than estimated. The count this row holds is the
        // provider's own, and dividing the new base by a measured ratio would put
        // a derived number where every other reader expects a measured one.
        if let Some(store) = &self.store {
            let _ = store.set_context(&self.cfg.session_id, None, None, None);
        }
        Ok(ForkReport {
            transcript_id: new_id,
            parent_id: old_id,
            forked_at,
            was_tokens,
            base_tokens: self.session.ledger.len(),
            truncated: outcome.truncated,
            tail_items: tail_items.len(),
            // The reason, in the wire's own shape — and `None` when there is no reason to
            // give, which is a re-seat and a re-ingest (`ForkTail::NONE` passes `""`).
            tail_why: (!tail.because.is_empty()).then(|| {
                letibot_sessionlog::event::CompactionTail {
                    turns: tail_turns.clone(),
                    carried: tail_items.len() as u64,
                    because: tail.because.to_string(),
                    dropped: tail_split.map(|s| s.dropped as u64).unwrap_or(0),
                }
            }),
            tail_dropped: tail_split.map(|s| s.dropped),
            tail_turns,
            tail_because: tail.because.to_string(),
        })
    }

    fn submit_item(&mut self, item: TranscriptItem) -> Result<Reply, HarnessError> {
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session
            .append_items(&self.engine, std::slice::from_ref(&item), &mut sink)?;
        self.reconcile(&mut sink, std::slice::from_ref(&item));
        self.persist()?;
        self.name_from_first_message();
        self.run_rounds()
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

    /// What a resume rebuilt, or `None` for a session opened fresh.
    pub fn resumed(&self) -> Option<&ResumeReport> {
        self.resumed.as_ref()
    }

    /// The notes `open` made about a session it did not resume — see the field.
    /// Empty for a resumed session, whose notes are in [`Harness::resumed`].
    pub fn open_notes(&self) -> &[String] {
        &self.open_notes
    }

    fn run_rounds(&mut self) -> Result<Reply, HarnessError> {
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
        // **The encoder for "is this turn getting anywhere".**
        //
        // Per user turn, because "already seen" is a claim about *this* turn — a
        // file read in an earlier turn is a legitimate thing to read again now.
        // `max_tool_rounds` below is the backstop it demotes; see `crate::progress`
        // for why a round count was the wrong instrument and what replaced it.
        let mut progress = crate::progress::ProgressDetector::new(self.cfg.stall_rounds);
        // Consecutive HTTP failures on the round being attempted. Not per turn:
        // the thing being waited out is the endpoint, and it is as likely to go
        // down on round nine as on round one.
        let mut attempt = 0u32;

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
            if round > 0
                && !self.compacting
                && let Some(window) = self.cfg.planning_window()
                && self.cfg.auto_compact
            {
                let resident = self.session.ledger.len() as u64;
                if resident + self.cfg.headroom() >= window {
                    // In the units the operator's header shows; see
                    // `Config::shown_tokens`. The ledger's own figures are named
                    // once at the end rather than per number, because three
                    // conversions in one sentence is not a sentence.
                    let (r, w, h) = (
                        self.cfg.shown_tokens(resident),
                        self.cfg.shown_tokens(window),
                        self.cfg.shown_tokens(self.cfg.headroom()),
                    );
                    let also = if self.cfg.tokens_are_converted() {
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
                             {model} since it took over, so nothing has measured how its \
                             tokens relate to the ledger's. One round sets it)",
                            model = self
                                .provider
                                .as_ref()
                                .map(|p| format!("{}/{}", p.name(), p.model()))
                                .unwrap_or_default()
                        )
                    } else {
                        String::new()
                    };
                    self.hub.publish(SessionEvent::Warning {
                        code: "context_wall".into(),
                        detail: format!(
                            "stopping this turn after {round} round(s): {r} of {w} \
                             tokens are resident and the next round needs {h} free{also}. \
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
                        "  {session_id}: context wall: stopped after {round} round(s) at {resident} of {window} tokens"
                    );
                    return Err(HarnessError::ContextWall {
                        rounds: round,
                        resident,
                        window,
                    });
                }
            }
            let mut sink = CapturingSink::new(self.hub.clone());
            // **Take the round again when the endpoint is the thing that
            // failed.** An inner loop, so a retry does NOT spend one of
            // `max_tool_rounds`: it produced nothing and appended nothing, and
            // ending a turn early because a server was restarting would be the
            // budget measuring the wrong thing. See `http_retry_after`.
            let outcome = loop {
                let mut steering = self.steering();
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
                    None => {
                        self.engine
                            .run_turn_steered(&mut self.session, &mut sink, &mut steering)
                    }
                    // A cloud turn: the transcript as messages, the ledger as the
                    // record. Same events, same verdicts, same TurnOk.
                    Some(p) => self.engine.run_turn_messages(
                        &mut self.session,
                        &mut sink,
                        &mut steering,
                        p.as_ref(),
                        &self.prefix.system,
                        &self.prefix.tools_json,
                        None,
                    ),
                };
                // The round has produced something or failed, either way the silence
                // is over and the watchdog has nothing left to say.
                watch.finish();
                let Err(TurnFailure::Http(e)) = attempted else {
                    attempt = 0;
                    break attempted;
                };
                let Some(wait) = http_retry_after(&e, attempt, self.cfg.http_retries) else {
                    break Err(TurnFailure::Http(e));
                };
                attempt += 1;
                // **Which host, asked of the thing that was contacted.** This read
                // `self.cfg.endpoint.authority()` — the LOCAL endpoint — on a path that
                // also serves cloud turns, so a resolver failure against a provider was
                // reported as a problem with `127.0.0.1:8080`. MEASURED 2026-10-01: the
                // operator read *"the model server at 127.0.0.1:8080 did not answer:
                // ... Temporary failure in name resolution"* and went to a `llama-server`
                // that was answering `/health` with 200. Not knowing which host failed is
                // what made the message expensive; `cfg.endpoint` is only right for the
                // local route, so the cloud route is asked for its own.
                let where_ = retry_host(self.provider.as_deref(), &self.cfg.endpoint);
                self.hub.publish(SessionEvent::Warning {
                    code: "model_endpoint_retry".into(),
                    detail: format!(
                        "the model endpoint at {where_} did not answer: {e}. Taking this round \
                         again in {:.0}s (attempt {attempt} of {}). \
                         Nothing was recorded, so the retry sends exactly the bytes this \
                         one did.",
                        wait.as_secs_f64(),
                        self.cfg.http_retries,
                    ),

                    compaction: None,
                });
                if !self.sleep_unless_closed(wait) {
                    break Err(TurnFailure::Http(e));
                }
            };
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
                    self.append_notice(&notices.join("\n"))?;
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
                Err(TurnFailure::EmptyLength { reason, .. }) => {
                    self.append_notice(&format!(
                        "You keep overthinking. Cut it short and do things. Your last \
                             turn spent its whole output on reasoning ({}) and nothing was \
                             recorded. Answer first. Reason after, or not at all.",
                        reason.as_str()
                    ))?;
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
                    self.append_notice(
                        "You keep overthinking. Cut it short and do things. Your last \
                             turn thought until it ran out and said nothing. Stop reasoning \
                             and act now.",
                    )?;
                    continue;
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
            if ok.metrics.prompt_tokens > 0 {
                self.cfg.ledger_scale =
                    Some((self.session.ledger.len() as u64, ok.metrics.prompt_tokens));
            }
            // Persisted with the ledger beside the count, so a restart recovers the
            // pair above instead of pairing the count with whatever ledger it has
            // then. See the gate in `Harness::open`.
            self.persist_context(&ok.metrics)?;
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
            // **What the agent says it is doing, recorded before its calls are
            // adjudicated.** A model's prose for a round is the sentence in front of
            // its tool calls — "the queued-prompt rendering lives in app.rs; I am
            // reading the composer block" — and that sentence is the one thing the
            // guard is missing when the operator's instruction names a goal rather
            // than a file. Never an authorisation and never citable; see
            // `ModelBrief::agent_claim`.
            self.trail.claims(&text);
            metrics.push(ok.metrics);

            if calls.is_empty() {
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
                    self.append_notice(&steer)?;
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
            for call in &calls {
                // Call order, and appended in call order. See point 3 above.
                // `self.tool_sink` is the log sink wrapped in the intent encoder, so
                // every result is measured on its way past — an `ok` is an effect
                // and anything else is an attempt, which is the distinction the
                // diff below is built on.
                let r = self.runtime.invoke(&turn_id, call, &mut self.tool_sink);
                let item = ToolRuntime::transcript_item(&r);
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
            let round_verdict = progress.end_round();
            let mut sink = CapturingSink::new(self.hub.clone());
            self.session
                .append_items(&self.engine, &results, &mut sink)?;
            self.reconcile(&mut sink, &results);
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
            if let CommandKind::OperatorShell { line, who } = &cmd.kind {
                let (line, who) = (line.clone(), who.clone());
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
                        "`{who}` ran `{name}` from their own console and the transcript could                          not be written: {e}. The call ran; what is missing is the record of                          it, which is what the next turn would have read."
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
        // **Not persisted from here.** The project row is `Sessions`' business and
        // is written on the between-turns path; a consented `allow-all` is not
        // written at all. This is the session's own point moving, and nothing else.
        let consented = *consented;
        match self.set_mode_consented(mode, consented) {
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

    fn steering(&self) -> HubSteering {
        HubSteering {
            hub: self.hub.clone(),
            trail: Some(self.trail.clone()),
            injected: self.injected.clone(),
            monitors: self.monitors.clone(),
            monitor_cursor: self.monitor_cursor.clone(),
            // The session's own set, so a child can be told at its round boundary what
            // nothing will wake it with — and a root's own set answers nothing here, so
            // R7's wake keeps its monopoly on the root's settlements.
            job_watch: self.job_watch.clone(),
            // **And the session's own children, so a stop takes them with it.** See the
            // interrupt arm of `try_next`.
            subagents: Some(self.subagents.clone()),
            held: VecDeque::new(),
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
        steer_for_turn(&self.intent, self.cfg.intent_prose, turn_id, items)
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

    fn append_notice(&mut self, text: &str) -> Result<(), HarnessError> {
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

    /// Attach content to the rows the log already announced (T13.1).
    ///
    /// The ids come off the events rather than being recomputed; see point 1 in the
    /// module header. A count mismatch is a bug in the pairing, not something to
    /// paper over, so it is announced as a warning on the same log the head reads.
    fn reconcile(&self, sink: &mut CapturingSink, items: &[TranscriptItem]) {
        // The trail's denominator, counted where every appended batch passes.
        // `messages_scanned` has to be what was **looked at**, and this is the one
        // place that knows how much there is.
        self.trail.note_items(items.len());
        reconcile_rows(&self.hub, sink, items);
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
        engine: &mut TurnEngine<'_>,
        session: &mut Session,
        sink: &mut CapturingSink,
        answerer: &letibot_turn::compaction::Answerer<'_>,
    ) -> Result<CompactionOutcome, HarnessError> {
        let mark = session.items.len();
        let outcome =
            run_compaction(engine, session, sink, answerer).map_err(HarnessError::Turn)?;
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
    fn flush_todos(&mut self) -> Result<(), HarnessError> {
        let v = self.todos.version();
        if v == self.todos_version {
            return Ok(());
        }
        self.todos_version = v;
        let items = self.todos.snapshot();
        if let Some(store) = &self.store {
            store
                .put_todos(&self.cfg.session_id, &items)
                .map_err(|e| HarnessError::Store(format!("todos: {e}")))?;
        }
        self.hub.publish(SessionEvent::TodosUpdated {
            todos: items.into_iter().map(Self::todo_entry).collect(),
        });
        Ok(())
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
            by: match t.by {
                letibot_tokencore::store::TodoBy::Model => letibot_sessionlog::event::TodoBy::Model,
                letibot_tokencore::store::TodoBy::Operator => {
                    letibot_sessionlog::event::TodoBy::Operator
                }
            },
            status: match t.status {
                letibot_tokencore::store::TodoStatus::Pending => WireTodoStatus::Pending,
                letibot_tokencore::store::TodoStatus::InProgress => WireTodoStatus::InProgress,
                letibot_tokencore::store::TodoStatus::Completed => WireTodoStatus::Completed,
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
        let Some(store) = &self.store else {
            self.persisted = self.session.ledger.rows().len();
            return Ok(());
        };
        let rows = self.session.ledger.rows();
        while self.persisted < rows.len() {
            let i = self.persisted;
            let row = &rows[i];
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
            store
                .append_item(&self.transcript_id, i as u32, item, row, tokens)
                .map_err(|e| HarnessError::Store(format!("row {i} ({}): {e}", row.item_id)))?;
            self.persisted += 1;
        }
        Ok(())
    }

    /// The round's prompt size onto the session row. See the call in `run_rounds`
    /// for why this is the number a head shows as the session's context.
    fn persist_context(&self, metrics: &TurnMetrics) -> Result<(), HarnessError> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store
            .set_context(
                &self.cfg.session_id,
                Some(metrics.prompt_tokens),
                Some(metrics.cached_tokens),
                // **The same ledger the in-memory pair above just used**, so a
                // restart recovers what this daemon had rather than a second
                // opinion about it. Both are taken AFTER the round's rows are
                // appended, so the two agree by construction.
                Some(self.session.ledger.len() as u64),
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
        let stored = match &self.cfg.provider {
            None => "local".to_string(),
            Some(pc) => match &pc.model {
                Some(m) => format!("{}/{}", pc.name, m),
                None => pc.name.clone(),
            },
        };
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
            if self.cfg.provider.is_none() {
                return None;
            }
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
}

/// **The stored ledger-to-provider pair, or `None` when it does not measure the
/// conversation in hand.**
///
/// The decision is a free function for the reason `sweep_abandoned_calls` gives
/// about its own: it can be tested without a daemon, a model or a socket. That is
/// not tidiness here — a provider cannot be pointed at a canned server (a
/// `ProviderConfig` names a vendor and nothing else), so a gate left inline in
/// `Harness::open` could only ever be exercised by a test that talks to
/// DeepSeek. The bug this replaces was found in production precisely because
/// nothing checked the pairing.
///
/// `ledger_now` is what this box counts for the conversation it has just rebuilt.
/// The stored ledger is what it counted for the prompt the provider's number
/// belongs to, and **equality is the whole test**: a ratio is a function of the
/// two SIZES, so a matching ledger size is exactly the equivalence the ratio
/// needs, and it is a stronger claim than the length alone suggests — content
/// that differed while measuring the same would still yield the right ratio.
///
/// It refuses in three cases, and each lands the safe way (an unscaled window,
/// which compacts EARLY):
///
///   * no stored pair — a row written before v12, or one whose turn never
///     finished. Unverifiable is not the same as wrong, but the two are the same
///     as unmeasured;
///   * a ledger that has moved since — rows appended, or a compaction forked the
///     transcript, either of which makes the provider's number describe a
///     conversation this one is not;
///   * a zero on either side, which is not a measurement of anything.
fn recovered_scale(
    stored_provider: Option<u64>,
    stored_ledger: Option<u64>,
    ledger_now: usize,
) -> Option<(u64, u64)> {
    let provider_tokens = stored_provider?;
    if provider_tokens == 0 || ledger_now == 0 {
        return None;
    }
    if stored_ledger != Some(ledger_now as u64) {
        return None;
    }
    Some((ledger_now as u64, provider_tokens))
}

/// **`0` is unbounded**, which is the default; see [`Config::max_tool_rounds`].
///
/// `usize::MAX` rather than a second loop shape, so there is one body and one
/// place a round is counted — two loops that had to stay in step is how the
/// `round + 1` in the reporting would come to mean two different things.
///
/// A named function rather than a `match` inline, because this is the whole of
/// "infinity" and it is one line that a test can hold to.
fn round_backstop(configured: usize) -> usize {
    match configured {
        0 => usize::MAX,
        n => n,
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
/// The board is one union (`TodoBoard::snapshot`), so this walks the whole of it and skips two
/// kinds: **a row with no condition is not this question's subject**, and **a running job is not
/// due**. Those two skips are the whole of the policy, and they are here rather than in the caller
/// so that the decision and the sentence [`fate_line`] writes for it cannot be tested apart from
/// each other — which is what left this piece unasserted until now.
fn due_rows(rows: &[TodoItem], fate: impl Fn(&str) -> JobFate) -> Vec<(String, String, JobFate)> {
    let mut out = Vec::new();
    for row in rows {
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

impl<'a> Harness<'a> {
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
        due_rows(&self.todos.snapshot(), |handle| self.job_fate(handle))
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
        // **A job whose output was redirected to a file: the FILE is the window** (R41).
        //
        // The capture is empty by construction — the daemon gave the bytes to the file — so
        // this used to answer with a sentence NAMING the path, and the operator's next ask was
        // the obvious one: *"job pane can tail the file"*. It can, and the DAEMON does the
        // reading: a head has no business reading the box's filesystem, and the path is this
        // side's own record of where it sent the bytes.
        //
        // `offset == 0` is the TAIL — the last `limit` bytes — because that is what an output
        // pane is for and what a fresh read means. Any other offset is a forward window from
        // there, so the ring's own convention holds: `→` asks for the offset `next` named, and
        // a reader can walk to the end of a long log.
        if slice.produced == 0
            && let Some(path) = letibot_tools::builtins::output_redirect_path(&view.command)
        {
            let state = view.state.word();
            let never_ran = view.state.never_ran();
            return Ok(match tail_file(&path, offset, limit) {
                Some(w) => JobWindow {
                    from: w.from,
                    to: w.to,
                    produced: w.produced,
                    // Everything before the window was not shown: the pane says how much.
                    dropped: w.from,
                    state,
                    never_ran,
                    lines: progress_lines(&w.text),
                    next: (w.to < w.produced).then_some(w.to),
                },
                // **The file is not there yet**, or it cannot be read. The sentence that names
                // it is still the honest answer, and it names the command that would read it.
                None => JobWindow {
                    from: 0,
                    to: 0,
                    produced: 0,
                    dropped: 0,
                    state,
                    never_ran,
                    lines: vec![format!(
                        "this job's output goes to `{path}` — the file is not there yet, or \
                         could not be read. `tail -n 50 {path}`"
                    )],
                    next: None,
                },
            });
        }
        Ok(JobWindow {
            from: slice.from,
            to: slice.to,
            produced: slice.produced,
            dropped: slice.dropped,
            state: view.state.word(),
            never_ran: view.state.never_ran(),
            lines: if slice.produced == 0 {
                // **Empty, and the redirected case cannot reach here** — a job whose command
                // redirects its output was answered from the FILE above. This used to name
                // the path in a sentence, which was the honest answer before the pane could
                // tail it; see the branch above.
                Vec::new()
            } else {
                progress_lines(&slice.text())
            },
            next: next_job_offset(&slice),
        })
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
/// Six, doubling from a second: 1, 2, 4, 8, 16, 32 — about a minute of waiting
/// before the turn fails for real. That is long enough to sit out the thing this
/// exists for (llama.cpp reloading a six-shard GGUF after `--sleep-idle-seconds`,
/// which answers `503 Loading model` for as long as it takes) and short enough
/// that an endpoint which is genuinely gone is reported rather than waited on.
///
/// **The default for [`Config::http_retries`], which is what the code reads.**
/// A caller that already KNOWS the endpoint is not there — a test pointed at a
/// dead port — sets it to 0 and is told so at once, instead of sitting out a
/// minute of waiting for a server it never wanted. That minute was real: seven
/// tests in `compact.rs` took 63.7 seconds of wall clock for 3.5 seconds of CPU,
/// and the whole difference was one of them walking this ladder.
pub const MAX_HTTP_RETRIES: u32 = 6;

/// **Is this failure worth taking the round again, and how long to wait first?**
///
/// `None` means no: report it. The operator's rule (2026-09-17) is *"when model
/// http endpoint doesnt answer or answers with error codes except
/// unauthenticated"*, and the exception is the point — a credential the server
/// rejected is rejected identically on every retry, so backing off on a 401 is a
/// minute of waiting to be told the same thing, with the real cause buried under
/// six notices.
///
/// What is retried, and the honest limits of the rule:
///
/// * **No answer at all** ([`HttpError::Io`]) — the connection was refused, or
///   dropped, or timed out. The commonest case on this box: the model server
///   restarting.
/// * **A status that is not 2xx**, except `401` and `403`. `403` is on the
///   exception with `401` because both mean *the credential is not the problem
///   the server has with you*, and neither improves by asking again.
/// * **A malformed response** — a stream that did not parse. Retried because the
///   likeliest cause is a truncated body from a server going down mid-answer.
///
/// **What this rule knowingly retries that will never succeed**: a `400` from a
/// prompt the server will not accept, and the `500 Context size has been
/// exceeded` that the mid-turn wall check exists to prevent. Both are
/// deterministic in the bytes, so all six attempts fail and the turn ends about
/// a minute late. That is the cost of following the operator's rule rather than
/// second-guessing which 5xx is which, and it is bounded — the alternative is a
/// list of "codes we think are transient" that is wrong the first time a new one
/// appears.
///
/// A turn is safe to take again because an HTTP failure commits NOTHING:
/// `stream_turn` posts before it accumulates, so the prompt is rebuilt from the
/// same ledger and the retry sends the same bytes.
fn http_retry_after(
    e: &letibot_turn::HttpError,
    attempt: u32,
    attempts: u32,
) -> Option<std::time::Duration> {
    if attempt >= attempts {
        return None;
    }
    let worth_it = match e {
        letibot_turn::HttpError::Io(_) => true,
        letibot_turn::HttpError::Malformed(_) => true,
        // **A 4xx is the request, not the weather.** The retry's whole premise is
        // that *"the retry sends exactly the bytes this one did"* — which is why
        // it is safe, and equally why it is pointless when the server's complaint
        // is about those bytes. A provider 400 was retried six times on identical
        // bytes with the waits doubling to 32 seconds, and the operator watched
        // every one of them.
        //
        // The two exceptions are the 4xx that are about timing rather than
        // content: 408 is the server saying it waited too long, 429 is it saying
        // not yet.
        //
        // **And a 429 is not always "not yet".** It is also the status a provider uses
        // when the account behind the key cannot pay for the call, and that one no retry
        // can lift — the body is the only place the difference is written. MEASURED
        // 2026-10-04 on the operator's GLM account: `http 429: 余额不足或无可用资源包,请充值。`
        // (insufficient balance, please top up), and the daemon took the round again six
        // times, the waits doubling to 32 seconds, on identical bytes against an account
        // with no credit — while the person watching pressed interrupt on a turn that
        // could not succeed.
        letibot_turn::HttpError::Status { code, body } => {
            !refused_for_want_of_credit(body) && (matches!(code, 408 | 429) || *code >= 500)
        }
    };
    worth_it.then(|| std::time::Duration::from_secs(1u64 << attempt))
}

/// **A refusal no retry can lift: the account cannot pay for the call.**
///
/// A 429 is the status a provider uses for both *not yet* (a rate limit) and *not ever until
/// somebody tops up* (a billing refusal), so the status alone cannot tell them apart and the
/// body can. MEASURED 2026-10-04: GLM answered `http 429: 余额不足或无可用资源包,请充值。` and
/// the retry loop took the round again six times — waits of 1, 2, 4, 8, 16 then 32 seconds —
/// while the operator pressed interrupt on a turn that could not succeed either way.
///
/// **Narrow on purpose.** Only phrases that name MONEY are here. A bare `quota` is left out
/// deliberately: *rate* quota is exactly the transient thing this retry path exists for, and
/// a list that swallowed it would stop retrying the case the whole exception was written
/// around. Nor is it a status code: the same 429 means both things, which is why the body is
/// the instrument.
fn refused_for_want_of_credit(body: &str) -> bool {
    const FILLED: &[&str] = &[
        "insufficient balance",
        "insufficient_quota",
        "insufficient funds",
        "insufficient credit",
        "please recharge",
        "no available resource package",
        "billing",
        // The provider's own words, which are what actually arrived: a reader who cannot
        // decode them would otherwise be told only that the endpoint "did not answer".
        "余额不足",
        "请充值",
        "资源包",
    ];
    let b = body.to_ascii_lowercase();
    FILLED.iter().any(|p| b.contains(p))
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
/// genuinely gone, and `http_retry_after` is where that policy lives. So this fires
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

/// A few words from the first message, as a session name.
///
/// Deliberately dumb: the first line, first six words, no model call, no
/// summarisation. A name is an **address** — the operator has to recognise it in a
/// list a week later — and a paraphrase is a worse address than the words that were
/// actually typed. A model-written title would also be a turn that costs a slot and
/// can fail, on the path where somebody is waiting for an answer.
///
/// Empty for a message with no words in it, and the caller then leaves the session
/// unnamed rather than storing a blank.
pub fn derive_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().take(6).collect();
    let mut out = words.join(" ");
    // Long enough to recognise, short enough for a picker row and a header.
    //
    // **This is a NAMING rule and its bound is the ADDRESS**, which is why it is small and
    // why nothing about a terminal belongs here: the title is also the fabric alias
    // (`@seat/title`), and a person has to be able to type it. A string that is only ever
    // *displayed* takes a different rule — see [`subagent_title`].
    const MAX: usize = 48;
    if out.chars().count() > MAX {
        out = out.chars().take(MAX - 1).collect::<String>();
        out.push('…');
    }
    out
}

/// **How long a subagent's title may be** — a display bound, not an address bound.
///
/// The operator, 2026-10-03: *"subagents list has everything capped at 48 chars. but this
/// cap is a ui concern — for example my terminal is 160 cells wide, make the cap 256."*
///
/// 256, and the head trims the row to the width it actually has. The number is generous on
/// purpose: the daemon does not know the terminal, so the only job left for a bound here is
/// stopping something pathological, and 256 is far past any width a row can show.
pub const SUBAGENT_TITLE_MAX: usize = 256;

/// **The subtask, as the subagent's title.**
///
/// **Not [`derive_title`], and the difference is the whole of a defect.** That one is a
/// naming rule whose bound is the fabric ADDRESS — six words, 48 characters, a string a
/// person types — while a subagent's title is *not* an address (nothing addresses a child
/// by name; a child hears the room through its parent) but **a row in the pane**, and the
/// only cap that belongs on a row is the width of the terminal, which the daemon cannot
/// know. Reusing the address rule here cut every row to a name's length; the operator read
/// the result as *"everything capped at 48 chars"*.
///
/// So the task's first line is kept whole up to [`SUBAGENT_TITLE_MAX`] and the head trims.
/// The row the operator actually reads is drawn from
/// [`letibot_sessionlog::SessionEvent::Subagent::task`] — whole, on every state — so this
/// title is what names the child's own session and appears in the picker; it is generous
/// for the same reason.
fn subagent_title(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out = line.to_string();
    if out.chars().count() > SUBAGENT_TITLE_MAX {
        out = out.chars().take(SUBAGENT_TITLE_MAX - 1).collect::<String>();
        out.push('…');
    }
    out
}

/// The visible text of a batch of appended items.
fn visible_text(items: &[TranscriptItem]) -> String {
    let mut out = String::new();
    for i in items {
        if let TranscriptItem::Assistant { text, .. } = i {
            out.push_str(text);
        }
    }
    out
}

/// The real `task` runner: spawns a **persistent subagent session** and runs it to
/// completion, returning the subagent's final answer to the parent.
///
/// A subagent is a full session — its own hub (an operator can attach and message
/// it), its own store row (it survives the daemon and can be resumed), and its own
/// harness — not an ephemeral nested turn. It inherits the parent's permission
/// ruleset and mode, so its calls are governed by the same allow/deny/ask rules, and
/// what reaches an `ask` is posted to the tree's ROOT: the operator's ruling is that a
/// subagent's ask *"should surface to the parent head all the way to the root
/// obviously"*, so [`crate::answers::SubagentAdjudicator`] is what it opens with.
///
/// The subagent seats [`Seat::Coder`] (read, write, edit, grep, glob) — nothing
/// that spawns further subagents — so delegation is one level by construction, not
/// by convention.
/// One spawned subagent, as the parent can see it: where it has got to, and a
/// door to knock on until it gets further.
///
/// A `Condvar` rather than a poll loop, for the same reason [`letibot_tools::exec`]
/// gives a job one: a `task_result` with a `timeout_ms` should wake when the
/// child answers, not on the next tick of somebody's chosen interval.
pub(crate) struct TaskSlot {
    state: std::sync::Mutex<letibot_tools::builtins::task::TaskStatus>,
    settled: std::sync::Condvar,
    /// **Whether this subagent was stopped rather than finishing**, which is the one
    /// thing the child's own result cannot say.
    ///
    /// An interrupted turn still returns text — whatever it had written when the
    /// interrupt landed — so `run_to_completion` comes back `Ok`, and a `task_result`
    /// that called that `Done` would hand the parent a truncated answer under the name of
    /// the child's last word. The flag is set by [`HarnessTaskRunner::kill`], before the
    /// interrupt is sent, so the settlement that follows cannot be read as an answer.
    killed: std::sync::atomic::AtomicBool,
    /// **Which session started this child** — the one field supervision needs, because a
    /// tree's handle list is shared and the runner that owns a child is the only one that may
    /// stop it (see [`HarnessTaskRunner::stop_all`]).
    ///
    /// `task_result` deliberately reads the WHOLE list, so any ancestor can collect any
    /// handle in its tree — a decision made on purpose and unchanged. Stopping is the other
    /// direction and goes to parents, which is what this field says: one list, one owner per
    /// entry.
    owner: String,
    /// **Whether the child's own thread has ended.**
    ///
    /// Not the same question as [`TaskSlot::state`], and the difference is the whole of what
    /// `stop_all` has to get right: a child that has ANSWERED is still alive (a host child
    /// parks in `serve_child` and can be asked more), so a `Done` slot is a child that still
    /// owns whatever it started and must still be stopped. This flag is set where the child's
    /// thread leaves `run_to_completion`, and it is what makes `stop_all` skip a child that is
    /// really gone instead of submitting an interrupt nothing will read.
    exited: std::sync::atomic::AtomicBool,
}

impl TaskSlot {
    fn new(owner: &str) -> TaskSlot {
        TaskSlot {
            state: std::sync::Mutex::new(letibot_tools::builtins::task::TaskStatus::Running {
                note: None,
            }),
            settled: std::sync::Condvar::new(),
            killed: std::sync::atomic::AtomicBool::new(false),
            owner: owner.to_string(),
            exited: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// **This child's thread has ended.** Read by [`HarnessTaskRunner::stop_all`], which is
    /// the only caller that has to tell "already answered" from "still here".
    fn mark_exited(&self) {
        self.exited.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn has_exited(&self) -> bool {
        self.exited.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The child's own last word about what it is doing. Kept only while it is
    /// running: a note on a settled subagent would overwrite its answer.
    fn note(&self, text: &str) {
        let mut g = self.state.lock().expect("task slot");
        if let letibot_tools::builtins::task::TaskStatus::Running { note } = &mut *g {
            *note = Some(text.to_string());
        }
    }

    /// Marked before the interrupt goes out, so a settlement that arrives a microsecond
    /// later cannot be recorded as the child having answered.
    fn kill(&self) {
        self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// **The status as the slot holds it**, for a test that has to assert a settlement happened —
    /// or did not overwrite an earlier one. `collect` is the real reader.
    #[cfg(test)]
    fn status(&self) -> letibot_tools::builtins::task::TaskStatus {
        self.state.lock().expect("task slot").clone()
    }

    fn settle(&self, status: letibot_tools::builtins::task::TaskStatus) {
        // **`Done` under a kill is not an answer.** See the field: the text is what the
        // child had written when it was stopped, and reporting it as the child's word is
        // the same defect as reporting a truncated summary as whole.
        let status = match status {
            letibot_tools::builtins::task::TaskStatus::Done { .. }
                if self.killed.load(std::sync::atomic::Ordering::SeqCst) =>
            {
                letibot_tools::builtins::task::TaskStatus::Failed {
                    why: "stopped by `job_kill` — this is not an answer".into(),
                }
            }
            other => other,
        };
        *self.state.lock().expect("task slot") = status;
        self.settled.notify_all();
    }
}

/// **One child's answer, settled exactly once, whatever path the turn takes out of the call.**
///
/// `run_to_completion` has several early exits (`fail(...)`) and one long tail, and what it
/// settles is what `task_result` reads: a settlement that never fires leaves a parent waiting for
/// ever, and a second one lets a later failure overwrite the answer the child actually gave. So
/// the settlement belongs to a guard rather than to each exit — [`AnswerOnce::say`] records the
/// answer and `Drop` covers every path that did not.
///
/// **Why it moved here at all.** It used to be the caller's: `start`'s thread settled from this
/// function's RETURN, which is right while the function ends with the turn — and wrong the moment
/// a child stays reachable afterwards, because the park does not end until the hub does. So the
/// answer is given as the turn ends and the serving begins after it.
struct AnswerOnce<'a> {
    slot: &'a TaskSlot,
    said: std::cell::Cell<bool>,
}

impl<'a> AnswerOnce<'a> {
    fn new(slot: &'a TaskSlot) -> Self {
        AnswerOnce {
            slot,
            said: std::cell::Cell::new(false),
        }
    }

    /// **The first answer is the answer.** A later one is another path out of the same call, not
    /// a second opinion about how the turn went.
    fn say(&self, status: letibot_tools::builtins::task::TaskStatus) {
        if self.said.replace(true) {
            return;
        }
        self.slot.settle(status);
    }
}

impl Drop for AnswerOnce<'_> {
    fn drop(&mut self) {
        if !self.said.get() {
            self.slot
                .settle(letibot_tools::builtins::task::TaskStatus::Failed {
                    why: "this subagent's turn ended without saying how it went — the record is \
                          short of an answer, not in possession of one"
                        .into(),
                });
        }
    }
}

/// **What a child does with a command that reached its queue between turns.**
///
/// A free function so the decision can be read and tested without a model, a hub or a socket —
/// the same reason [`subagent_depth_refusal`] is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildCommand {
    /// Answer it: this is the operator (or a head) asking the child something, and a child is an
    /// agent with a session of its own — the operator's ruling in one line: *"subagent is just
    /// another agent. its 'sub' is a way to inherit something and be managable. so mid turn, post
    /// turn whatever"*.
    Answer,
    /// **Leave, and stop this session's children on the way out.** See [`ChildCommand::Stop`].
    Leave(&'static str),
    /// **A stop, and a stop walks the subtree downward.** See the supervision invariant on
    /// [`HarnessTaskRunner`]: a parent owns its children's lifecycle, so the last thing this
    /// session does is stop what it started, and then it goes.
    Stop,
}

fn child_command(kind: &letibot_sessionlog::CommandKind) -> ChildCommand {
    use letibot_sessionlog::CommandKind as K;
    match kind {
        K::Prompt { .. } => ChildCommand::Answer,
        // **An interrupt IS this child's stop, whether or not a turn is running.** The turn it
        // was written for is the common case and it never arrives here (a running turn takes its
        // own interrupt through the steering poll); what DOES arrive here is the case that was
        // measured: a parent stopped while this child sat between turns with children of its
        // own still computing. Saying *nothing is running to interrupt* was true of the session
        // and false of its subtree, which is the whole of what a supervision tree is for.
        K::Interrupt { .. } => ChildCommand::Stop,
        // A message's contract is a turn IN FLIGHT — the parent's own `task_message` refuses to
        // send one to a child that is not running — so one here is the race that refusal names.
        // Answering it as a prompt would answer, in the parent's name, something the parent
        // deliberately did not send.
        K::Message { .. } => ChildCommand::Leave("a message steers a turn in flight"),
        // The rest is a session's own machinery — a compaction, a re-seat, a mode, the operator's
        // door — and a child inherits all of it from its parent rather than owning a copy.
        _ => ChildCommand::Leave("a door a child does not own"),
    }
}

/// **The sentence a session leaves about the children it stopped on the way down** — or
/// `None` when it had none.
///
/// Pure, so what a stop SAYS can be asserted without a daemon — the same reason
/// [`subagent_depth_refusal`] and [`child_command`] are free functions. It is one function
/// because there are three callers (the steering source's interrupt arm, a child's own serving
/// loop, and the daemon's between-turns arm) and a sentence written three times is a sentence
/// that drifts.
///
/// **A refusal is named, not counted away.** A child that could not be stopped is the one
/// fact a reader needs, and "stopped 3 subagents" over a refusal is exactly the claim-versus-
/// fact defect this tree keeps paying for.
fn children_stopped_notice(results: &[(String, Result<String, String>)]) -> Option<String> {
    if results.is_empty() {
        return None;
    }
    let mut stopped = Vec::new();
    let mut refused = Vec::new();
    for (handle, said) in results {
        match said {
            Ok(_) => stopped.push(handle.clone()),
            Err(why) => refused.push(format!("`{handle}` — {why}")),
        }
    }
    let mut out = String::new();
    if !stopped.is_empty() {
        out.push_str(&format!(
            "stopping this session stopped the {} subagent(s) it was still running, before \
             stopping itself: {}. They stop their own children in turn, so the whole subtree \
             goes; `task_result` on any of them says it was stopped.",
            stopped.len(),
            stopped.join(", ")
        ));
    }
    if !refused.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "{} subagent(s) could NOT be stopped and are still running:\n  {}",
            refused.len(),
            refused.join("\n  ")
        ));
    }
    Some(out)
}

/// **Stop every child this session owns, and say so on its own log.**
///
/// The downward edge of a supervision tree, in one place, for the three doors a stop arrives
/// through: the steering source's interrupt arm (a turn in flight), [`serve_child`]'s stop arm
/// (a child between turns), and `Sessions::dispatch`'s interrupt arm (a session the daemon holds
/// and that has nothing running). One function, so a stop cannot mean different things at
/// different doors — the same reason `subagent_depth_refusal` is one.
///
/// It publishes on the session's own hub and not the children's: the reader who has to know
/// that this session stopped its subtree is the one looking at this session — the operator who
/// pressed Esc, or the parent that killed it.
fn stop_children_first(
    runner: Option<&Arc<dyn letibot_tools::builtins::task::TaskRunner>>,
    hub: &Hub,
) -> Option<String> {
    let runner = runner?;
    let said = children_stopped_notice(&runner.stop_all())?;
    hub.publish(letibot_sessionlog::SessionEvent::Warning {
        code: "subagents_stopped".into(),
        detail: said.clone(),
        compaction: None,
    });
    Some(said)
}

/// **A child's own turns, after the task it was spawned for** — and the door its parent's
/// stop comes through.
///
/// The door is the one the first prompt came through — [`Harness::submit`] — and that is the whole
/// of the honesty here: a child's turn IS that call (see `run_to_completion`), so a later prompt is
/// not a second, smaller `run_prompt`. What a child does not get is what it never had:
/// `Sessions::run_prompt`'s per-turn machinery belongs to the sessions the daemon opened, and a
/// child's harness borrows `Parts` built on this thread — which is why the daemon cannot serve it
/// (recorded, with the four hazards, on the row that would do that properly).
///
/// **It is the child's own SERVANT, and that is what makes it a session the daemon can serve.**
/// Something this child owns settling is queued on the child's own watch set and the ring names the
/// child — who the daemon cannot run a turn for. So the daemon hands the wake HERE
/// ([`Hub::wake_its_own_reader`] through `Sessions::wake`), and this loop answers it with
/// [`Harness::wake`]: drain what you own, run the turn it makes. The alternative is what R58
/// measured happening — a settlement that rings a bell with no worker behind it.
///
/// It ends when the hub is closed — the same liveness test `jobwatch::watch_task` uses for the
/// thread it parks per child — and **nothing here is on a clock**: a child asked something an hour
/// later answers it.
fn serve_child(sub: &mut Harness<'_>, hub: &Hub, sub_id: &str) {
    use letibot_sessionlog::hub::OwnWork;
    loop {
        let kind = match hub.take_own_work() {
            // The hub closed: the daemon is going away, or this session was reaped.
            OwnWork::Closed => return,
            // **A wake has no command behind it and needs none**: its whole content is
            // *something this session owns has settled*, and the settlement is in this session's
            // own queue. `Harness::wake` takes it and runs the turn it makes — or finds it already
            // taken at a round boundary, says `Ok(None)` and runs nothing, which is what stops one
            // settlement being delivered twice.
            OwnWork::Wake => {
                if let Err(e) = sub.wake() {
                    hub.publish(letibot_sessionlog::SessionEvent::Warning {
                        code: "wake_failed".into(),
                        detail: format!(
                            "{sub_id} was woken because a subagent it started had finished, \
                             and the turn that would have read the settlement failed: {e}. \
                             The settlement is still in this session's queue and the next \
                             wake will find it."
                        ),
                        compaction: None,
                    });
                }
                continue;
            }
            OwnWork::Command(cmd) => cmd.kind,
        };
        match child_command(&kind) {
            ChildCommand::Answer => {
                let text = match &kind {
                    letibot_sessionlog::CommandKind::Prompt { text } => text.clone(),
                    _ => unreachable!("`child_command` answers a Prompt and nothing else"),
                };
                // **The same door the first prompt took** — see `run_to_completion`.
                // A steering message to a parked child is a prompt like any other,
                // and a child near the wall must compact on ITS overrun exactly as
                // on its first: the operator's rule is about the session, not about
                // which prompt it was.
                if let Err(e) = sub.submit_as_a_normal_session(&text) {
                    // **Said, and the child stays reachable.** A turn that failed is the same fact
                    // `turn_failed` reports for a root; a child has no worker to publish it, so it
                    // goes on the child's own log — which its parent and the operator both read.
                    hub.publish(letibot_sessionlog::SessionEvent::Warning {
                        code: "turn_failed".into(),
                        detail: format!(
                            "{sub_id} was asked something after its task and the turn \
                             failed: {e}. The child is still here and can be asked again."
                        ),
                        compaction: None,
                    });
                }
            }
            // **The children first, then this session.** Stopping is a tree's downward edge — see
            // the supervision invariant on [`HarnessTaskRunner`] — and this is where a child that
            // is between turns receives it. It then leaves: an interrupt that reaches a session
            // stops that session, and a stopped session that stayed would be the shape this whole
            // change is against. Its exit is reported upward the way every child's exit is, by the
            // slot its parent's watcher is blocked on.
            ChildCommand::Stop => {
                sub.stop_children();
                return;
            }
            ChildCommand::Leave(why) => eprintln!(
                "  {sub_id}: left a command in this subagent's queue — {why}. A child's \
                 machinery is its parent's, so nothing was run."
            ),
        }
    }
}

/// **The session's children, and the whole of the supervision edge between them.**
///
/// The operator's design, in their words: *"think about it like it is an erlang supervision
/// tree. we talk to parents and they own lifecycle."* Four sentences are the whole of it, and
/// every one of them is a line somewhere below:
///
///   1. **A parent owns its children's lifecycle.** A child is started by one session
///      ([`HarnessTaskRunner::start`]) and that session is its owner — [`TaskSlot::owner`] —
///      which is what lets a runner stop exactly what it started and not its siblings' work or
///      its own grandchildren.
///   2. **An exit goes parent-ward, never to the root.** What a child's settlement is queued
///      on is the session that STARTED it (`jobwatch`'s own routing), never an ancestor's.
///      The parent decides what it means — collect it, act on it, or finish — and when the
///      parent itself finishes, ITS exit is what travels the next level up. That is why the
///      operator is never handed a grandchild's exit, and why nothing here needs a parent
///      pointer.
///   3. **Termination walks the subtree downward.** A session that is stopped stops its
///      children FIRST ([`TaskRunner::stop_all`], called from the interrupt doors and from a
///      child's own serving loop) and only then goes itself; each child does the same, so the
///      walk is one level per session and no runner needs to know its grandchildren.
///   4. **Depth is a bound, not a prohibition.** `task` is seated at every level and refused at
///      the cap by NAME — see [`subagent_depth_refusal`].
///
/// # What is NOT here, and is the second half of the design
///
/// **TODO(supervision-2, the operator's second half): the RESTART POLICY.** Nothing in this
/// tree decides what a child's exit *means* beyond reporting it: a parent that wants a child
/// restarted starts one again itself, and there is no `one_for_one`/`one_for_all`, no
/// max-restarts-in-a-window, and no supervision strategy on the parent. The seam for it is the
/// parent's own drain of its child's settlement — the notice `Harness::wake` builds — because
/// that is the one place a parent already decides something about a child that has ended. The
/// same half adds a configurable `max_depth`; today the bound is `--max-subagent-depth`
/// ([`subagent_depth_refusal`]), which is the number the operator's design calls for and is
/// left exactly as it is rather than invented a second time.
#[derive(Clone)]
struct HarnessTaskRunner {
    /// The shared pieces, held as `Arc` so the runner is `'static` while the parent
    /// harness still borrows them. Reassembled into a temporary [`Parts`] inside
    /// [`HarnessTaskRunner::run`] for the sub harness to borrow.
    vocab: Arc<Vocab>,
    wiring: Arc<Wiring>,
    mode_store: Arc<std::sync::RwLock<crate::modes::ModeStore>>,
    /// The daemon's session registry, so a subagent is a real session an operator
    /// can see and attach rather than a turn hidden inside the parent's.
    registry: Arc<letibot_sessionlog::registry::Registry>,
    /// The parent's configuration, already resolved (workspace, mode, title) by
    /// [`Harness::open_with_registry`] before this was built. Cloned as the base for
    /// each sub session; only the id, title and seat are overridden.
    base: Config,
    /// The shared subagent journal, so a spawn and its finish reach the dashboard's
    /// state file.
    tasks: Arc<crate::tasks::TaskJournal>,
    /// The loaded skills and LSP config, shared with the sub session's `skill`/`lsp`
    /// tools so a subagent sees the same capabilities the parent does.
    skills: Arc<letibot_tools::builtins::skill::SkillRegistry>,
    lsp: Arc<letibot_tools::builtins::lsp::LspConfig>,
    /// The subagents this session has started, in the order it started them.
    /// Shared with every clone of this runner — the thread that runs a child
    /// holds one, and so does the tool that collects it.
    slots: Arc<std::sync::Mutex<Vec<(String, Arc<TaskSlot>)>>>,
    /// **The watcher set a child of THIS session joins** (R58).
    ///
    /// A slot rather than a value because the runner is built before the harness has
    /// built its own `job_watch`, and the runner must outlive the assignment — so the
    /// harness fills this once, a few lines later, with the set a child should join.
    /// For a root that set is the root's own; for a child it is the child's, which carries the
    /// tree's ROOT (for a permission card) and the level above (for what this session cannot
    /// drain, and for the ring that says so) — so a `Parts` built for a grandchild inherits both
    /// without a parent-chain walk.
    tree_watch: Arc<std::sync::Mutex<Option<Arc<crate::jobwatch::JobWatchers>>>>,
    /// **The head's answers a child of THIS session asks through** — the ask route's
    /// half of R58, and the same slot shape as `tree_watch` for the same reason: the
    /// runner is built before the gate that creates them.
    ///
    /// For a root this is the `Answers` of its own head adjudicator; for a child it is
    /// the one inherited from the tree, which is the ROOT's — so a grandchild's card
    /// reaches the same head a child's does, with no parent-chain walk. `None` is a
    /// tree with no head-wired adjudicator at all, and a child then refuses by name
    /// rather than posting a card nothing can settle.
    head_answers: Arc<std::sync::Mutex<Option<Arc<crate::answers::Answers>>>>,
    /// **The daemon's own server window, as this session last knew it** — `None` until
    /// the session first switches away from local, and the server's window once it has.
    ///
    /// A cell shared with the [`Harness`] because a spawn may ask for `model: "local"`
    /// while the parent is on a cloud provider: the child then runs on the daemon's
    /// server and must plan its compaction against THAT window, not the cloud model's —
    /// the same mistake `retune_window` exists to prevent, one direction over. The
    /// harness writes it wherever it updates its own copy; the runner reads it at spawn.
    local_window: Arc<std::sync::Mutex<Option<Option<u64>>>>,
    /// **This session's point, as it stands NOW** — the one thing `base` cannot answer.
    ///
    /// `base` is a clone taken at open and nothing updates it: a `/mode` mid-session
    /// moves `Harness::cfg.mode` and leaves `base.mode` where it was, so a child built
    /// from `base` came up at the point its parent *opened* at. Measured 2026-10-06: the
    /// operator's own session at `allow-all` — consented, on their box — and every child
    /// of it at `automode-edits`, each child's `bash` putting a person in front of a
    /// question the parent had already answered, with the decision row reading as the
    /// child's own. Written by [`Harness::set_mode_consented`]; read by the spawn, which
    /// hands it to the child as `sub_cfg.mode`.
    point: Arc<std::sync::Mutex<letibot_tools::mode::Mode>>,
    /// **The `task_start` children this session has put in a tree**, by handle — the ask each
    /// was given and the placement it works in.
    ///
    /// A record rather than a second look at the child: the enqueue needs the brief and the
    /// placement, and by the time a child has finished its own session may be gone, its
    /// workspace torn down, and its transcript the only place the ask survives — which is not
    /// a place a queue can read from. So the two facts are kept here, where the runner that
    /// arranged the tree can answer for them, and the entry is built from them.
    ///
    /// **Keyed by handle, and the handle is the entry's id.** That is what makes *one entry
    /// per finished child* true across the two notices (the child's own settlement and a
    /// `task_result` that arrives later): both look the same child up and write the same row.
    placed: Arc<std::sync::Mutex<Vec<PlacedChild>>>,
    /// **Where a finished child's entry is written** — the session store, opened on first use.
    ///
    /// Opened lazily and held as a cell for [`crate::jobwatch::JobRecorder`]'s reasons, which
    /// are the same two: a store opened at construction is opened for every session that never
    /// starts a `task_start` child, and a `rusqlite::Connection` is `Send` and not `Sync`, so a
    /// runner that must be both keeps it behind a mutex rather than in the struct's own hand.
    merge_store: Arc<std::sync::Mutex<Option<Store>>>,
}

/// **One `task_start` child, as the runner remembers it** — see
/// [`HarnessTaskRunner::placed`].
///
/// The two fields are the two things the enqueue needs and nothing else in this process can
/// answer for: the ask the child was given (the reviewer's only framing) and where its branch
/// is. The path is inside the placement rather than beside it because the entry's `worktree`
/// column and the placement's `path` are one fact.
#[derive(Debug, Clone)]
struct PlacedChild {
    /// The handle `task_result` collects by, and the id the entry is minted under.
    handle: String,
    /// The prompt the child was spawned with, verbatim.
    brief: String,
    /// Where the child was put: the path, the branch and the base SHA.
    placement: letibot_tools::builtins::task::WorktreePlacement,
}

impl PlacedChild {
    /// **The record [`HarnessTaskRunner::start_worktree`] keeps for a child, or `None` for
    /// one the queue has no business with.**
    ///
    /// One rule, and it is a rule about the MAIN checkout: **a `task_start` child put in the
    /// main tree is not recorded.** It works on `main`'s own branch, so there is no branch of
    /// its own to land and no base to land it on — [`crate::mergequeue::entry_for_finished`]
    /// refuses one anyway, and a record here would only be a second place that has to know
    /// it. Everything else — a fresh worktree, with a branch cut from the base the caller
    /// named — is recorded, because it is exactly the shape the queue serves.
    ///
    /// **A function rather than three assignments at the call site**, for the reason every
    /// other seam in this tree is one: the recording IS half of the enqueue (the `placed`
    /// lookup in [`HarnessTaskRunner::enqueue_entry`] reads what this wrote), and a test that
    /// built a `PlacedChild` of its own would be asserting against a copy of this rule rather
    /// than against it. See `queue_e2e`, which calls this and then the enqueue.
    fn of(
        handle: &str,
        brief: &str,
        placement: &letibot_tools::builtins::task::WorktreePlacement,
    ) -> Option<PlacedChild> {
        if placement.main_tree {
            return None;
        }
        Some(PlacedChild {
            handle: handle.to_string(),
            brief: brief.to_string(),
            placement: placement.clone(),
        })
    }
}

/// **A subagent's model, resolved from the `task` tool's `model` argument.**
///
/// The operator's ask, 2026-10-05: *"I want to be able to have subagents using different
/// models. say you deepseek should be able to run local model"*, and the other direction
/// too: *"local qwen in main session should be able to run cloud glm"*. **Symmetric on
/// purpose** — the child's model has nothing to do with the parent's, so the two names are
/// resolved by one function and neither direction is special.
///
/// The resolution goes through [`crate::slash::models_choice`], the same door `/models`
/// uses, and that is the point rather than a convenience: a name a person can type is a
/// name a child can run on, the key lookup is the same one the picker's green rows use,
/// and a name neither accepts is refused by the SAME sentence. A separate resolver here
/// would be a second answer to *what is this provider* — the class of defect this tree
/// keeps finding by looking for it.
///
/// `local_window` is the daemon's own server window as the parent last knew it
/// ([`HarnessTaskRunner::local_window`]), used for `local`; a cloud name takes its window
/// from the catalogue, and a name the catalogue has no row for leaves the number alone
/// rather than inventing one.
#[derive(Debug)]
pub struct SubagentModel {
    /// What the record says: `local`, or `PROVIDER/MODEL`.
    pub label: String,
    /// `None` is the daemon's own server; `Some` a cloud provider, its key already found.
    pub provider: Option<crate::config::ProviderConfig>,
    /// The window to plan compaction against, `None` when nobody knows one.
    pub window: Option<u64>,
    /// **A declared local model the child runs on**, instead of the daemon's own server.
    ///
    /// The operator, 2026-10-05: *"models from providers.toml must be usable for
    /// subagents"*. `provider` cannot carry one — it is `None` for local and a cloud
    /// preset otherwise — and overloading `None` to mean *either* the daemon's server
    /// *or* some other box is the same conflation `ModelChoice` was widened to end.
    ///
    /// The child still tokenizes with the daemon's vocabulary, so the spawn path runs
    /// [`local_model_vocab_refusal`] against it exactly as a session switch does.
    pub local: Option<letibot_provider::keys::LocalModel>,
}

/// **The child's window, or the refusal that names the knob** — pure, so both halves are
/// testable without a harness, and beside [`subagent_depth_refusal`] because it is the same
/// shape of decision: *refuse by NAME rather than by absence*, so the model always sees `task`
/// and the sentence says which knob moves.
///
/// **Why this exists, measured 2026-10-05.** Three children of one session ran on this daemon's
/// own server (`qwen-3.8-27b`, 256k) while their config carried the PARENT's `context_window`
/// (deepseek's catalogue ~1M) and the parent's `ledger_scale`, which [`Config::planning_window`]
/// scales that window by — so their wall was computed four times their own, `should_compact` was
/// faithful to it and therefore never fired, and they reached 911,522 / 910,486 / 911,708
/// resident tokens **with no compaction item in their logs at all**. The spawn resolved a model
/// only when one was NAMED, and an absent `model:` was assumed to mean "the parent's" — but what
/// answers such a child is the daemon's own server whenever this session sits on a provider.
///
/// `Some(Some(n))` is a named window. `Some(None)` is *somebody looked and the server did not
/// say*, and `None` is *nobody has an opinion*: for the child those are the same answer, because
/// `None` is not a large window — it is no wall at all, and every check that would compact reads
/// `let Some(window) = …` and is skipped.
fn child_window_refusal(label: &str, window: Option<Option<u64>>) -> Option<String> {
    match window {
        Some(Some(_)) => None,
        _ => Some(format!(
            "nothing was spawned on `{label}`: nobody can name a window for it — not this \
             daemon's own server and not the catalogue `/models` reads — and a child planned \
             against no window is not a child with room: nothing would compact it and it would \
             run to the server's own limit. Name a model that has one"
        )),
    }
}

/// **Whether a child will be answered by the same model as its parent** — the one condition
/// under which the parent's window is the child's window rather than a number belonging to a
/// different server. `None` on both sides is the daemon's own server, which is the same server
/// for both.
///
/// This exists so the refusal above is not BROADER than the defect it was written for. The
/// defect was a child inheriting a window from a model it was not running on; a child that runs
/// on exactly the parent's provider and model inherits nothing foreign, and it is the case the
/// catalogue cannot answer at all: a session started with an explicit `--context-window` for a
/// model the catalogue does not carry has a real number, and the operator stated it.
fn runs_on_the_parents_model(
    child: &Option<crate::config::ProviderConfig>,
    parent: &Option<crate::config::ProviderConfig>,
) -> bool {
    match (child, parent) {
        (None, None) => true,
        (Some(c), Some(p)) => c.name == p.name && c.model == p.model,
        _ => false,
    }
}

/// **How a child samples, as the project's `leticode.toml` says it does.**
///
/// The operator: *"when used in subagents mode we need to call qwen with the parameters
/// similar to dense78 - otherwise it overthinks terribly"*. `role` is the seat the child
/// was seated on, and the parameters are `[roles.<seat>]` over `[roles.subagent]` over the
/// dense78 numbers the operator measured — see `LeticodeConfig::parameters_for`.
///
/// **They REPLACE what this config already carries rather than merging onto it**, the way a
/// `[model.*]` block replaces the built-in: the project file is the more specific statement
/// about this project's subagents, and a merge would keep the parent's greedy literal
/// (`temperature = 0.0`, `top_k = 1`) in force under a profile that named only
/// `presence_penalty` — the built-in deciding the most consequential knob in a profile
/// somebody wrote.
///
/// **`sampling` and no other field.** The operator's second sentence is *"we dont have the
/// same context limit as with dense obviously"*, so the child's window stays whatever was
/// measured for the model it actually runs on: `context_window` is not touched here, and a
/// project file has no way to state one at all (`LeticodeConfig` carries models and
/// samplers, not walls). This function is one line wide so that stays true.
fn apply_spawn_parameters(
    sub_cfg: &mut Config,
    leticode: &crate::leticode_config::LeticodeConfig,
    role: &str,
) {
    sub_cfg.sampling = leticode.parameters_for(role).to_sampling();
}

/// See [`SubagentModel`]. `want` is `local`, a declared local model, or
/// `PROVIDER[/MODEL]`.
pub fn subagent_model(
    want: &str,
    local_window: Option<Option<u64>>,
) -> Result<SubagentModel, Vec<String>> {
    subagent_model_in(None, want, local_window)
}

/// [`subagent_model`] against a named `providers.toml`.
///
/// The file is a parameter for the reason [`crate::slash::models_choice`]'s is: the
/// declared local models are read from it, so a test that cannot name one is a test
/// of whatever happens to be in the operator's config. The public entry point passes
/// `None`, which is the real file.
pub fn subagent_model_in(
    file: Option<&std::path::Path>,
    want: &str,
    local_window: Option<Option<u64>>,
) -> Result<SubagentModel, Vec<String>> {
    let want = want.trim();
    if want == "local" {
        return Ok(SubagentModel {
            label: "local".into(),
            provider: None,
            // The daemon's own server window: the number the parent remembered on its way
            // out to a provider, or whatever `None` means — *nobody measured one* — rather
            // than the cloud model's number wearing the local child's name.
            window: local_window.flatten(),
            // `local` names a DECLARED model; bare `local` is the daemon's own server,
            // which needs no declaration and takes none.
            local: None,
        });
    }
    let (provider, model) = match want.split_once('/') {
        Some((p, m)) if !p.is_empty() && !m.is_empty() => (p, Some(m)),
        _ => (want, None),
    };
    let (pc, _notes) =
        crate::slash::models_choice(provider, model, None, file).map_err(|lines| {
            let mut out = lines;
            out.push(format!(
                "nothing was spawned on `{want}`; `local` needs no name, and a provider's \
                 name is the one `/models` lists"
            ));
            out
        })?;
    let pc = match pc {
        crate::slash::ModelChoice::Metered(pc) => pc,
        // **A declared local model seats a child like any other model** — the
        // operator's ruling: *"models from providers.toml must be usable for
        // subagents"*. `provider` stays `None`, because this IS the local path; what
        // changes is where that path points, and `local` carries it.
        //
        // The window comes from the block when it states one and is left `None`
        // otherwise, for the caller to measure off that server's `/props`. This
        // function does no network: its docstring promises it is testable without a
        // harness, and a probe in here would make every test of it either a lie or a
        // live test.
        crate::slash::ModelChoice::Local(m) => {
            return Ok(SubagentModel {
                label: m.name.clone(),
                provider: None,
                window: m.profile.window,
                local: Some(m),
            });
        }
        // `local` is handled above by name, so reaching here means the store spelled a
        // local switch some other way — a resolver bug, refused rather than guessed.
        crate::slash::ModelChoice::OwnServer => {
            return Err(vec![format!(
                "`{want}` resolved to no provider and no model"
            )]);
        }
    };
    let preset = letibot_provider::Preset::parse(&pc.name).map_err(|e| vec![e])?;
    let cat = letibot_provider::catalogue::Catalogue::load();
    let window = preset.window(pc.model.as_deref(), &cat);
    let label = format!(
        "{}/{}",
        pc.name,
        pc.model
            .clone()
            .unwrap_or_else(|| preset.default_model(&cat))
    );
    Ok(SubagentModel {
        label,
        provider: Some(pc),
        window,
        local: None,
    })
}

/// **The settings row's `mode_source` for a subagent** — the vocabulary the row already uses
/// for where a point came from, and the clamp when there was one.
///
/// A free function because it is a sentence, and because the two spellings have to stay one
/// vocabulary: the row a head draws for a child says *inherited from the parent session* —
/// the phrase this file already used for it — and a child that could not carry the point its
/// parent is at adds the sentence `Mode::inherited_by` wrote, which names the seat, the point
/// inherited, what was missing and where it landed.
///
/// **The clamped case is the whole reason this exists.** A child silently at a different
/// point than its parent is the state that made this a bug hunt: the operator's session was
/// at `allow-all` and every child came up at `automode-edits` with nothing anywhere saying
/// the two were not the same.
fn child_mode_source(why: Option<&str>) -> String {
    const INHERITED: &str = "inherited from the parent session";
    match why {
        None => INHERITED.to_string(),
        Some(why) => format!("{INHERITED}, clamped to what this seat can carry: {why}"),
    }
}

/// **The R58 depth refusal, as one pure function so it is testable without a harness.**
///
/// `Some(message)` refuses a `task` call that would open a level past `max`;
/// `None` is *go*. Two design decisions are the whole of it:
///
/// * **It refuses by NAME and not by absence.** `task` stays seated on `m2_coder` at
///   every depth, so the model always sees the tool; the refusal says which knob moves
///   the limit. A seat that simply lacked `task` at the cap manufactures the workaround
///   this tree has already paid for once (`m2_coder`'s note on `todo`: 13 calls and
///   ~15k tokens emulating a tool that had not been given).
/// * **It is assessed at the CALL and not on the seat**, because a seat can say only
///   *names `task`* (unlimited) or *does not* (never), and the ruling wants a
///   configured number in between.
///
/// A `max` of 0 refuses every `task` call, which is the honest reading of *no nesting*.
///
/// **TODO(supervision-2): `max_depth`, and it is the second half of the design rather than a
/// missing knob.** The cap is IN FORCE tonight — `Config::max_subagent_depth`, default 3,
/// `--max-subagent-depth` — and the half that is not built is a *runtime* depth the tree can be
/// told at spawn (per-call, and per-child, so one branch can be told to go three deep while its
/// sibling stops at one) together with the restart policy beside it. Depth is a BOUND here and
/// never a prohibition: `task` is seated at every level and this refuses by naming the knob, so
/// a model that hits it learns what to change rather than inventing a way round. Do not add a
/// second number before that decision is made — the operator's design names one bound and this
/// is it.
fn subagent_depth_refusal(depth: u32, max: u32) -> Option<String> {
    if depth < max {
        return None;
    }
    Some(format!(
        "this session is at subagent depth {depth} and `--max-subagent-depth` is {max}, so \
         `task` here would open depth {}. Nothing was started. Raise \
         `--max-subagent-depth` (or start the run with it higher) if the tree should go \
         deeper.",
        depth + 1,
    ))
}

/// **Run a git command in a directory and return its stdout, trimmed.**
///
/// The one door `task_start`'s filesystem work goes through: the tool layer has no
/// git and no exec of its own, so the runner is where `git worktree add` and its
/// companions are run. `Ok` is the command's stdout, trimmed; `Err` is the command
/// and its stderr, so a refusal names what was tried and what git said rather than
/// swallowing the failure.
fn git_in(dir: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {} could not be run: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// **A subagent's session id, minted so that two children cannot share one.**
///
/// leticl found this one on the wire and diagnosed it without reading this tree: three `task`
/// calls minted in a loop got ONE id, *"so they cost one pane row, no subagent counter, one
/// `[task]` notice — and **two of the three answers never reach me at all**"*. The id was
/// `"{parent}-sub-{milliseconds}"`, and a millisecond clock is not an id source at all: three
/// calls in one round land in the same millisecond trivially. It is now the entropy this daemon
/// already mints its own session id from ([`crate::config::now_ns`], the value behind
/// [`crate::config::Config::session_id`]) with the counter loop `server.rs::mint_session_id`
/// already uses for a root session — the same shape one prefix along, rather than a second
/// scheme.
///
/// **Why a duplicate is not cosmetic.** The id is the HANDLE the model collects by
/// (`task_result`) and the key every head folds on, and `task_result` finds the FIRST slot
/// under a handle — so the other replies exist, settle, and are unreachable. That is the same
/// failure `ToolOutcome` refuses to collapse into *success with an empty payload*, one layer
/// down: a child's whole reply, silently unaddressable.
///
/// **A finer clock would not fix it; the guard is what does.** The operator's framing: *"a
/// timestamp is enough for a browser, not for a harness with automated loops"*, and *"nanos
/// would shrink the window and keep the race; a counter, a per-session sequence, or a random
/// suffix removes it"*. So there are two checks, because either alone misses the case that was
/// actually hit:
///
///   * the **registry** catches an id whose child has been adopted;
///   * `handed_out` catches one whose child is still being OPENED — and that is the ordinary
///     shape, not an edge: `adopt` happens inside the child's thread, after a whole harness has
///     been opened, while the mint returns immediately. Three `task` calls in one round
///     therefore all mint before any of them adopts.
///
/// **The timestamp stays and an ordinal disambiguates.** The timestamp is what keeps a subagent
/// minted after a daemon restart from colliding with one already in the STORE — a counter
/// resets, and `adopt` checks only the live registry, so a collision there would RESUME the old
/// child instead of spawning a new one. So the nanosecond is the starting point and the suffix
/// is added while the id is taken.
fn mint_sub_id(
    registry: &letibot_sessionlog::registry::Registry,
    parent: &str,
    handed_out: impl Fn(&str) -> bool,
) -> String {
    mint_sub_id_at(registry, parent, crate::config::now_ns(), handed_out)
}

/// [`mint_sub_id`] with the clock stated rather than read, so the collision can be driven in a
/// test without racing a nanosecond.
fn mint_sub_id_at(
    registry: &letibot_sessionlog::registry::Registry,
    parent: &str,
    ns: u128,
    handed_out: impl Fn(&str) -> bool,
) -> String {
    let mut id = format!("{parent}-sub-{ns}");
    let mut n = 1u32;
    while registry.get(&id).is_some() || handed_out(&id) {
        n += 1;
        id = format!("{parent}-sub-{ns}-{n}");
    }
    id
}

impl letibot_tools::builtins::task::TaskRunner for HarnessTaskRunner {
    /// **Start the child and come back.** The body below is the work, and it
    /// runs on its own thread: the daemon executes a round's calls in order on
    /// one thread, so a `task` that waited for its child held every call behind
    /// it for the child's whole life — fifteen minutes, measured 2026-09-17, of
    /// four other calls sitting unfinished while the operator and the model both
    /// read it as a hang.
    ///
    /// What is NOT deferred is the failure to start: a bad role, a spec that does
    /// not parse. Those are answered by this call, because they are facts about
    /// the request rather than about the child.
    fn start(
        &self,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
    ) -> Result<String, String> {
        // **The depth cap, and it is refused by NAME rather than by absence** (R58).
        // `task` is seated on `m2_coder` unconditionally, so a model always sees the
        // tool and learns *why* it cannot use it here; the alternative — stripping
        // `task` from the seat at the limit — is the workaround this tree has already
        // paid for once (the `todo` note on `m2_coder`: 13 calls and ~15k tokens
        // emulating a tool that had not been given). `depth` is the session's own level,
        // so this refuses the spawn that would create level `depth + 1`.
        if let Some(why) = subagent_depth_refusal(self.base.depth, self.base.max_subagent_depth) {
            return Err(why);
        }
        // **The id is minted here rather than inside the body** — it is what this call returns
        // and the body has not run yet — **and it is minted UNIQUELY, which is the half that
        // was missing.** See [`mint_sub_id`] for the three-children-one-id defect.
        let sub_id = mint_sub_id(&self.registry, &self.base.session_id, |id| {
            self.slots
                .lock()
                .expect("task slots")
                .iter()
                .any(|(h, _)| h == id)
        });
        // The seat is checked NOW: a role this build does not know is a fact
        // about the call, and answering it from a thread would report "started"
        // for something that never could.
        let role = spec.role.as_str();
        if !(role.is_empty() || role == "coder") {
            Seat::parse(role)?;
        }
        let slot = Arc::new(TaskSlot::new(&self.base.session_id));
        self.slots
            .lock()
            .expect("task slots")
            .push((sub_id.clone(), slot.clone()));

        let me = self.clone();
        let prompt = prompt.to_string();
        let spec = letibot_tools::builtins::task::TaskSpec {
            role: spec.role.clone(),
            downgrade: spec.downgrade.clone(),
            placement: spec.placement,
            model: spec.model.clone(),
            // The worktree `task_start` arranged, when it did: the child's workspace is
            // the worktree, not the parent's tree. `None` for a plain `task`.
            worktree: spec.worktree.clone(),
        };
        let id = sub_id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!(
                "subagent-{}",
                letibot_sessionlog::registry::short_id(&sub_id)
            ))
            .spawn(move || {
                let slot2 = slot.clone();
                // **The settlement is the function's own now** (see `AnswerOnce`): it happens as
                // the turn ends, because what follows — a host child serving its own queue until
                // the hub closes — does not end until the daemon does. There is nothing for this
                // thread to do with the answer; the slot already has it.
                let _ = me.run_to_completion(&id, &prompt, &spec, &slot, &mut |n| slot2.note(n));
                // **And this is where the child's thread ends — the fact stopping needs.** See
                // [`TaskSlot::exited`]: a child that answered is still alive (it parks and can be
                // asked more), so `stop_all` cannot read "has an answer" as "is gone".
                slot.mark_exited();
            });
        if let Err(e) = spawned {
            // Nothing is running; say so rather than handing back a handle for a
            // child that was never started.
            self.slots
                .lock()
                .expect("task slots")
                .retain(|(h, _)| h != &sub_id);
            return Err(format!("the subagent thread could not be started: {e}"));
        }
        Ok(sub_id)
    }

    /// **Arrange the tree, then start the child** — the `task_start` door.
    ///
    /// The order is the point: the worktree is created *before* the child is
    /// spawned, and a path that already exists is refused by name and without
    /// spawning anything. The operator's ask, in their words: *"we will need a
    /// new tool - task_start or what that will arrange worktree, firecode and
    /// subagent"*. The two mistakes this prevents are a child spawned into the
    /// main tree and a child killed instead of corrected for being in the wrong
    /// place — so the placement is a fact the spawn carries (see
    /// [`letibot_tools::builtins::task::TaskSpec::worktree`]), not a thing the
    /// caller remembers to say.
    ///
    /// **The default is never the main tree.** A `task_start` that did not name
    /// the main checkout creates a fresh worktree at
    /// `<workspace>/.claude/worktrees/agent-<slug>` on branch `agent/<slug>`, cut
    /// from the base the caller named (or the repo's HEAD). The main tree is the
    /// exception, and it requires the explicit `main_tree` argument.
    ///
    /// **The build cache is shared deliberately.** The worktree's cargo is pointed
    /// at the main tree's `target/` (via a `.cargo/config.toml` the runner writes
    /// into the worktree — the same mechanism as `CARGO_TARGET_DIR`, and the one
    /// that survives the child's own environment being cleared), because cargo's
    /// own lock serialises concurrent builds and a fresh worktree otherwise pays a
    /// cold build of the whole graph.
    ///
    /// **The branch is the deliverable, and it is never pushed.** This method does
    /// not push, and will not: the branch is to be landed by a merge queue.
    ///
    /// **TODO(merge-queue): the merge queue does not exist yet.** The branch is
    /// left where it is and the answer says so, rather than inventing an interface
    /// for a queue that is not built. When the queue lands, this is the door it
    /// takes.
    fn start_worktree(
        &self,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
        worktree: &letibot_tools::builtins::task::WorktreeSpec,
    ) -> Result<letibot_tools::builtins::task::WorktreeHandle, String> {
        use letibot_tools::builtins::task::{
            WorktreeHandle, WorktreePlacement, slug_from_prompt, worktree_branch, worktree_path,
        };
        // **The depth cap, refused by name before any tree is arranged** — the same
        // check [`Self::start`] makes, done here first so a child that cannot be
        // spawned does not get a worktree created for it.
        if let Some(why) = subagent_depth_refusal(self.base.depth, self.base.max_subagent_depth) {
            return Err(why);
        }
        let workspace = self.base.workspace.display().to_string();
        let slug = if worktree.slug.is_empty() {
            slug_from_prompt(prompt)
        } else {
            worktree.slug.clone()
        };
        let base = worktree.base.clone().unwrap_or_else(|| "HEAD".to_string());

        let placement = if worktree.main_tree {
            // **The exception**: the child works in the main checkout. No tree is
            // arranged; the placement is the main tree's own path, branch and HEAD.
            let branch = git_in(&workspace, &["rev-parse", "--abbrev-ref", "HEAD"])
                .map_err(|e| format!("the main tree's branch could not be read: {e}"))?;
            let base_sha = git_in(&workspace, &["rev-parse", "HEAD"])
                .map_err(|e| format!("the main tree's HEAD could not be read: {e}"))?;
            WorktreePlacement {
                path: workspace.clone(),
                branch,
                base_sha,
                main_tree: true,
            }
        } else {
            // **The default**: a fresh worktree, arranged before the child is spawned.
            let path = worktree_path(&workspace, &slug);
            let branch = worktree_branch(&slug);
            // **The refusal of an existing path, by name and without spawning
            // anything.** The tool pre-checks this; the runner re-checks it because
            // it is the one that creates the path, and a check the creator does not
            // make is a check that can be raced.
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(format!(
                    "`{path}` already exists, so a fresh worktree cannot be created there. \
                     Nothing was arranged and nothing was spawned. Use a different `slug`, \
                     or `main_tree: true` to work in the main checkout (the exception)."
                ));
            }
            // **The base SHA, read before the branch is cut**, so the answer names the
            // commit the branch actually starts from rather than a guess.
            let base_sha = git_in(&workspace, &["rev-parse", &base])
                .map_err(|e| format!("the base `{base}` could not be resolved: {e}"))?;
            // **The worktree, created before the child is spawned.** A failure here is
            // a spawn that fails, named — never a child run in the parent's tree and
            // called placed.
            git_in(
                &workspace,
                &["worktree", "add", &path, "-b", &branch, &base],
            )
            .map_err(|e| {
                format!(
                    "the worktree could not be created at `{path}`: {e}. Nothing was \
                     spawned. The base was `{base}` and the branch would have been \
                     `{branch}`."
                )
            })?;
            // **The build cache, shared deliberately.** The worktree's cargo is pointed
            // at the main tree's `target/`, so the child does not pay a cold build of
            // the whole graph. Written into the worktree rather than the child's
            // environment because the child's environment is cleared before its own
            // pairs, and a config file survives that.
            let cargo_dir = std::path::Path::new(&path).join(".cargo");
            let _ = std::fs::create_dir_all(&cargo_dir);
            let cargo_config = cargo_dir.join("config.toml");
            let _ = std::fs::write(
                &cargo_config,
                format!(
                    "# Written by `task_start`: the build cache is shared with the main \
                     tree, so this worktree does not pay a cold build of the whole graph.\n\
                     [build]\ntarget-dir = \"{}\"\n",
                    std::path::Path::new(&workspace).join("target").display()
                ),
            );
            // **And the config file must not be able to land in a commit or in the
            // operator's `git status`.** It is a build-cache pointer, not a change: a
            // config file that shows up as an untracked change in every review is a
            // change nobody made, reviewable by nobody. So it is excluded in the
            // worktree's own `info/exclude` — per-worktree, so the main tree's status
            // and every other worktree are untouched, and nothing is added to the
            // repo's `.gitignore` that the operator did not ask for.
            if let Ok(git_dir) = git_in(&path, &["rev-parse", "--git-dir"]) {
                let exclude = std::path::Path::new(&git_dir).join("info").join("exclude");
                if let Some(parent) = exclude.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let line = format!(".cargo/config.toml\n");
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&exclude)
                    .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
            }
            WorktreePlacement {
                path,
                branch,
                base_sha,
                main_tree: false,
            }
        };

        // **The placement rides the spec into the spawn**, so the child's workspace is
        // the worktree (see `run_to_completion`), and the handle the tool returns names
        // the path, branch and base SHA the child was put in.
        let mut spec = spec.clone();
        spec.worktree = Some(placement.clone());
        let handle = self.start(prompt, &spec)?;
        // **And the two facts the enqueue will need are kept, now that both exist.** The
        // handle is minted by `start`, and the placement and the ask are in hand here — so
        // this is the only moment at which *which child*, *which branch* and *what was asked*
        // can be recorded together. Which children are recorded at all is
        // [`PlacedChild::of`]'s rule, so the end-to-end test beside this file exercises the
        // same one rather than a copy of it.
        if let Some(record) = PlacedChild::of(&handle, prompt, &placement) {
            self.placed.lock().expect("placed children").push(record);
        }
        Ok(WorktreeHandle { handle, placement })
    }

    /// **Stop a subagent by interrupting the turn it is running.**
    ///
    /// The child spends its whole life inside `sub.submit`, and the one door that reaches
    /// a turn already running is that session's own steering — the same door a head's
    /// Esc-esc uses, addressed to the child's hub rather than to this session's. The hub
    /// is reachable because a subagent is a real registered session (that is what lets an
    /// operator attach to one), and it is found by the handle the model was given.
    ///
    /// **The address is the daemon's, and the daemon is the one caller the hub admits without a
    /// seat** — see [`letibot_sessionlog::hub::DAEMON_SUBMITTER`], whose arm in `Hub::submit`
    /// admits this name for an interrupt and for nothing else. `expected_seq` is `0`, which the
    /// hub reads as *no expectation*: a stop has nothing to say about where the reader is, and a
    /// stale mark would refuse the kill.
    ///
    /// **AND THE ANSWER DECIDES THE RETURN.** This function used to call `hub.submit` with a
    /// name the hub did not know and then format whatever came back into a sentence beginning
    /// *"interrupted the turn"* — so every subagent kill answered `Rejected { reason: "not
    /// attached" }` to a caller that was told the opposite. MEASURED 2026-10-05: the operator's
    /// three subagents were *"stopped"*, went on running, and their row counts grew (102 to 108)
    /// while the kill was in flight. A tool may not report a state change the daemon refused; it
    /// is the same claim-versus-fact rule the rest of this tree holds to, and it is why the
    /// match below is exhaustive over the daemon's answer rather than over our hopes.
    fn kill(&self, handle: &str) -> Result<String, String> {
        let slot = self
            .slots
            .lock()
            .expect("task slots")
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, s)| s.clone());
        let Some(slot) = slot else {
            return Err(format!(
                "no subagent `{handle}` in this session; `task_result` with no argument \
                 lists the ones there are"
            ));
        };
        let Some(hub) = self.registry.get(handle) else {
            return Err(format!(
                "`{handle}` has already settled, or its session is gone, so there is no \
                 turn left to interrupt. Nothing was stopped."
            ));
        };
        let f = hub.submit(
            letibot_sessionlog::hub::DAEMON_SUBMITTER,
            &format!(
                "job_kill-{}",
                letibot_sessionlog::registry::short_id(handle)
            ),
            0,
            letibot_sessionlog::CommandKind::Interrupt {
                reason: "`job_kill` stopped this subagent; whatever it had written is \
                         incomplete"
                    .into(),
            },
        );
        match &f {
            letibot_sessionlog::ServerFrame::Accepted { .. } => {
                // **Marked only once the daemon said yes**, and the order is the point. The flag
                // is what makes `task_result` say *stopped* rather than *answered*; setting it
                // for an interrupt that was refused would make a running subagent look settled,
                // and setting it before the submit is how the old version could label a child
                // that was never touched. The child may still settle between the submit and
                // this line — with the acceptance in hand that reads as the stop it is, because
                // its turn is no longer running.
                slot.kill();
                Ok(format!(
                    "the daemon accepted the interrupt for `{handle}`, so its turn stops at its \
                     next boundary. A subagent is stopped by interrupting its turn, not by \
                     reaping a process, so there is no cgroup and no pid to report. \
                     `task_result` on it now says it was stopped rather than answered."
                ))
            }
            other => Err(format!(
                "`job_kill` did NOT stop `{handle}`: the daemon refused the interrupt and \
                 answered {other:?}. Nothing was stopped — do not read this as a stop, and the \
                 same spelling will reach the same answer."
            )),
        }
    }

    /// **Say something to a child that is still working** — the operator's ruling, 2026-10-06:
    /// *"in the tree all subagents must be addressable by their parents. that is how live
    /// corrections delivered."*
    ///
    /// The door is the child's own hub, exactly as [`Self::kill`]'s is, and the address is
    /// `DAEMON_SUBMITTER` for the same reason: the parent is seated in another session's hub
    /// entirely. What it submits is a [`letibot_sessionlog::CommandKind::Message`] rather than a
    /// `Prompt`, and that difference is the feature — the child hears an agent's correction at
    /// its next round boundary, recorded as an agent's, instead of something the operator is
    /// supposed to have typed.
    ///
    /// **REFUSED BY NAME WHEN THE CHILD IS NOT RUNNING, and that is a measurement rather than
    /// caution.** A message rides the same queue as every other command, so a submission against
    /// a child whose turn has ended answers `Accepted` and then sits there: nothing drains a
    /// child's queue between turns, so the parent would be told it had corrected a child that
    /// never hears a word of it. The operator met the other end of this: *"message was queued
    /// when you stopped and it didnt restart you"*. So the live turn is the precondition, and it
    /// is checked rather than assumed — the race it cannot close (the turn ending between this
    /// check and the drain) is named on the child's log by the worker's own arm.
    fn send(&self, handle: &str, text: &str) -> Result<String, String> {
        let Some(hub) = self.registry.get(handle) else {
            return Err(format!(
                "no subagent `{handle}` in this session, or its session is gone. `task_result` \
                 with no argument lists the ones there are."
            ));
        };
        // **The live turn is the precondition.** See this function's docstring: a message to a
        // child between turns is `Accepted` by the hub and read by nobody.
        if !hub.status().running {
            return Err(format!(
                "`{handle}` is not running a turn, so there is nothing to steer: a message is \
                 delivered INTO the turn it is meant to correct, and a subagent between turns \
                 would never hear one. Read it with `task_result` instead, or start a new \
                 subagent with the correction in its prompt. Nothing was sent."
            ));
        }
        let f = hub.submit(
            letibot_sessionlog::hub::DAEMON_SUBMITTER,
            &format!(
                "task_message-{}",
                letibot_sessionlog::registry::short_id(handle)
            ),
            0,
            letibot_sessionlog::CommandKind::Message {
                from: self.base.session_id.clone(),
                text: text.to_string(),
            },
        );
        match &f {
            letibot_sessionlog::ServerFrame::Accepted { .. } => Ok(format!(
                "`{handle}` was told, as `{}`: it hears this at its next round boundary and \
                 keeps working.",
                self.base.session_id
            )),
            // The same rule `kill` follows, and for the same reason: a tool may not report a
            // state change the daemon refused.
            other => Err(format!(
                "`task_message` did NOT reach `{handle}`: the daemon answered {other:?}. \
                 Nothing was sent."
            )),
        }
    }

    fn collect(
        &self,
        handle: &str,
        timeout: std::time::Duration,
    ) -> letibot_tools::builtins::task::TaskStatus {
        let slot = self
            .slots
            .lock()
            .expect("task slots")
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, s)| s.clone());
        let Some(slot) = slot else {
            return letibot_tools::builtins::task::TaskStatus::Unknown;
        };
        let g = slot.state.lock().expect("task slot");
        let (g, _) = slot
            .settled
            .wait_timeout_while(g, timeout, |s| {
                matches!(s, letibot_tools::builtins::task::TaskStatus::Running { .. })
            })
            .expect("task slot");
        g.clone()
    }

    fn started(&self) -> Vec<String> {
        self.slots
            .lock()
            .expect("task slots")
            .iter()
            .map(|(h, _)| h.clone())
            .collect()
    }

    /// **The child has finished, so its branch goes to be landed** — the merge queue's enqueue,
    /// on the runner that arranged the tree.
    ///
    /// The operator's ask, in their words: *"we need a gated merge to main, and worktree
    /// cleanup"*, and *"a permanent subagent that lazily starts as soon as task_start was
    /// finished and item put into the queue"*.
    ///
    /// # Why this is a runner's act and not a tool's
    ///
    /// The row needs the ask the child was given, the placement it worked in, and the store the
    /// queue lives in — and the tool layer has none of the three. The runner arranged the tree,
    /// so the runner is the one that can hand them over; see
    /// [`letibot_tools::builtins::task::TaskRunner::finished`] for the seam itself.
    ///
    /// # Two doors, one entry
    ///
    /// This is called from two places, and they are one act rather than two answers:
    ///
    ///   * **The child's own settlement**, inside `run_to_completion` — the true moment the
    ///     child finished. A host child parks after its turn and serves its own queue until the
    ///     daemon closes it, so *the thread ending* is not the finish; the answer being given is
    ///     (see `AnswerOnce`).
    ///   * **`task_result` on a `Done`** — the tool layer's one sight of a finish, and the door
    ///     that makes this callable by a fake runner in a test.
    ///
    /// The two write **the same row under the same id** (the child's handle), and `put_merge_entry`
    /// is an upsert, so the second is a no-op that reports the same entry. That is deliberate:
    /// the alternative — one door only — is either a branch that never lands because nobody
    /// collected the child, or a queue that depends on the model remembering to call
    /// `task_result`.
    ///
    /// # What is refused, and what is not
    ///
    /// * **A plain `task` child** answers [`Finished::NoBranch`]: it works in the parent's own
    ///   tree and has no branch of its own. Not an error — the ordinary case.
    /// * **A child of the main checkout** answers the same, by name: there is no branch to land.
    /// * **A session with no `--store`** cannot be enqueued at all, and that is a refusal rather
    ///   than a quiet `NoBranch`: the queue lives in `sessions.db`, so a daemon without one has
    ///   no queue, and a parent told *queued* about a branch in no queue is the lie this tree
    ///   keeps refusing to tell.
    fn finished(&self, handle: &str) -> Result<letibot_tools::builtins::task::Finished, String> {
        use letibot_tools::builtins::task::Finished;
        match self.enqueue_entry(handle)? {
            Some(entry) => Ok(Finished::Queued {
                id: entry.id,
                branch: entry.branch,
            }),
            None => Ok(Finished::NoBranch {
                why: format!(
                    "`{handle}` left no branch of its own to land — it was not a `task_start` \
                     child of this session, or it worked in the main checkout"
                ),
            }),
        }
    }

    /// **Stop every child this session still owns** — the downward edge of the tree.
    ///
    /// The operator's design, in their words: *"think about it like it is an erlang supervision
    /// tree. we talk to parents and they own lifecycle."* A session that is being stopped stops
    /// its children first, so nothing is left computing for nobody — which was measured, the
    /// night this was written: a stalled grandchild survived the parent that owned it, because
    /// the interrupt reached exactly one session.
    ///
    /// **Which children, and why each filter is a fact rather than a precaution**, is
    /// [`stoppable_children`]'s — pure, so it is asserted without a vocabulary on this box.
    /// What is left here is the delivery: [`Self::kill`] once per handle, which is the same door
    /// `job_kill` uses — the child's own turn is interrupted, or its serving loop is woken, and
    /// its OWN runner stops its children in turn. That is how one stop walks a whole tree
    /// without any session holding a pointer to its grandchildren.
    ///
    /// **What this is NOT: a restart policy.** Nothing here decides whether a child should be
    /// started again; see the TODO on [`HarnessTaskRunner`].
    fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
        let mine = stoppable_children(
            &self.slots.lock().expect("task slots"),
            &self.base.session_id,
        );
        mine.into_iter()
            .map(|handle| {
                let said = self.kill(&handle);
                (handle, said)
            })
            .collect()
    }
}

/// **Which of a session's slots are children IT may stop** — the ownership filter, on its own so
/// it can be read and asserted without a harness, a vocabulary or a live child.
///
/// Two filters, and each one is a fact rather than a precaution:
///
/// * **Owned by this session.** `slots` is the TREE's list (`Parts::tree_slots`), shared on
///   purpose so any ancestor can `task_result` any handle in its tree — but stopping is the other
///   direction, and a runner that stopped every entry it could see would stop its siblings'
///   children (and, from a root, its own children's children) in the name of a stop that was
///   never theirs. [`TaskSlot::owner`] is what tells them apart, and it is the field this whole
///   function exists for.
/// * **Not exited.** A child that has ANSWERED is still alive — a host child parks in
///   [`serve_child`] and can be asked more, and it may own grandchildren at that moment — so
///   "settled" cannot mean "gone". A child whose thread has ended is skipped: there is no
///   reader left for an interrupt, and a stop reported for one would be a claim with nothing
///   behind it.
fn stoppable_children(slots: &[(String, Arc<TaskSlot>)], me: &str) -> Vec<String> {
    slots
        .iter()
        .filter(|(_, slot)| slot.owner == me && !slot.has_exited())
        .map(|(handle, _)| handle.clone())
        .collect()
}

/// **Where a subagent's ask goes: the tree's ROOT, and the answers that settle it.**
///
/// One function because two decisions are one fact — *which session the card is posted to*
/// and *which sink can settle it* — and separating them is how a card ends up on a queue
/// whose only listener cannot answer it. It is split out of the spawn so the addressing can
/// be tested without a model turn, which is the only way anything inside that function can.
///
/// **The root is read, not walked for.** `tree` is the watcher set a child of this session
/// joins, and `tree_root` is the ROOT it inherited: the one session in a tree that has a HEAD —
/// the session `Sessions::open` holds and a card can be drawn by. **The card and the bell answer
/// two different questions and no longer share a field**: a bell now names the session that OWNS
/// a settlement (`jobwatch`'s module header), and a card needs the session that can ANSWER one,
/// so `tree_root` reads its own field. `None` is a session with no tree above it, and then its
/// parent IS the tree's root.
///
/// The hub is resolved by that id. `None` — a root this daemon does not hold, or a tree with
/// no head-wired adjudicator — is a refusal by name rather than a card nothing could settle.
fn subagent_ask_target(
    registry: &letibot_sessionlog::registry::Registry,
    tree: Option<&crate::jobwatch::JobWatchers>,
    parent: &str,
    answers: Option<&Arc<crate::answers::Answers>>,
) -> (String, Option<crate::answers::HeadAdjudicator>) {
    let root = tree
        .map(|w| w.tree_root().to_string())
        .unwrap_or_else(|| parent.to_string());
    let head = match (registry.get(&root), answers) {
        (Some(hub), Some(answers)) => {
            Some(crate::answers::HeadAdjudicator::new(hub, answers.clone()))
        }
        _ => None,
    };
    (root, head)
}

impl HarnessTaskRunner {
    /// **A runner with only the ENQUEUE half wired** — the seam the end-to-end test needs, and
    /// the whole of the visibility change that test required.
    ///
    /// [`HarnessTaskRunner`] is private and is built in exactly one place,
    /// [`Harness::open_with_registry`], which needs a vocabulary, a live subagent host and a
    /// gate. [`Self::enqueue_entry`] and [`Self::enqueue_finished`] need none of those three:
    /// they read four facts — `placed` (what [`Self::start_worktree`] recorded), `base.store`
    /// and `base.session_id`, `merge_store`, and `registry` — and the other eleven fields
    /// exist for the SPAWN. So before this, the glue that turns a finished child into a queue
    /// row was executed by no test at all: every piece it calls (`PlacedChild::of`,
    /// `entry_for_finished`, `Store::open`, `Store::put_merge_entry`, the broadcast) had a test
    /// of its own, and the wire between them was only read.
    ///
    /// **Why a constructor and not a widened API.** The alternative was `pub` on the struct,
    /// its fields and the two methods so that an integration test could reach them, which
    /// widens a daemon-internal type into the crate's public surface for the sake of a test.
    /// This does not: the struct, every field and both methods keep the visibility they had,
    /// and `#[cfg(test)]` means the function is not compiled into a binary at all. The test
    /// that uses it is `queue_e2e`, beside this file and in this crate for the same reason.
    ///
    /// `parts` is taken rather than assembled because the two fields nothing here reads
    /// (`vocab`, `wiring`) have no cheap stand-in — a [`Vocab`] is a loaded GGUF — and the
    /// tree's own precedent for that is `compact.rs`: a test that needs the vocabulary and
    /// not the server says so and skips when the GGUF is absent.
    #[cfg(test)]
    fn for_enqueue(
        parts: &Parts,
        base: Config,
        registry: Arc<letibot_sessionlog::registry::Registry>,
        placed: Vec<PlacedChild>,
    ) -> Self {
        let point = base.mode;
        Self {
            vocab: parts.vocab.clone(),
            wiring: parts.wiring.clone(),
            mode_store: parts.mode_store.clone(),
            registry,
            base,
            tasks: parts.tasks.clone(),
            skills: parts.skills.clone(),
            lsp: parts.lsp.clone(),
            slots: parts.tree_slots.clone().unwrap_or_default(),
            // The three cells a SPAWN reads and an enqueue never does: a tree's watcher set,
            // the head's answers, and the daemon's server window. A test that starts no child
            // has nothing to put in them, and `None` is what a root session starts with.
            tree_watch: Arc::new(std::sync::Mutex::new(None)),
            head_answers: Arc::new(std::sync::Mutex::new(None)),
            local_window: Arc::new(std::sync::Mutex::new(None)),
            point: Arc::new(std::sync::Mutex::new(point)),
            placed: Arc::new(std::sync::Mutex::new(placed)),
            merge_store: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// **This session's point, as it stands now** — see the field.
    fn point(&self) -> letibot_tools::mode::Mode {
        *self.point.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **The child has finished: enqueue its branch, and say so where a person is looking.**
    ///
    /// A thin wrapper around [`TaskRunner::finished`], and it exists for the two things a
    /// caller inside `run_to_completion` cannot do for itself:
    ///
    ///   * **It is infallible from the child's side.** The child's turn is over either way, and
    ///     a merge queue that could not be reached must not turn a finished subagent into a
    ///     failed one — the child's answer is already settled by the time this runs, and
    ///     `task_result` must keep returning it. So a refusal is printed on the parent's own log
    ///     (where the operator watching a tree is looking) rather than propagated.
    ///   * **It says what happened, unprompted.** A `task_start` child's branch being enqueued
    ///     is news the parent did not ask for and would otherwise learn only by calling
    ///     `task_result` — and a merge queue nothing announces is a merge queue nobody watches.
    ///
    /// # Two readers of one fact, and they go to two different places
    ///
    /// The `Warning` goes to **the parent's hub** and the `MergeEntryAdded` goes to **every
    /// session's log** ([`letibot_sessionlog::registry::Registry::broadcast`]), and the
    /// difference is the whole of the events decision:
    ///
    ///   * *Your child's branch is queued* is a fact about THIS conversation. It belongs in the
    ///     log the person reading this conversation is attached to, and nowhere else — the same
    ///     rule `Subagent` and `JobSettled` follow.
    ///   * *The queue now holds this entry* is a fact about the daemon, and a head attached to
    ///     any other session can open the queue pane. So the structured event is broadcast, and
    ///     the pane that folds it is the pane's own work.
    ///
    /// The enqueue itself is idempotent by id (the handle), so a later `task_result` on the
    /// same child is a no-op rather than a second entry — and the events are emitted once per
    /// call rather than once per entry, which is why the second notice is not a second
    /// `MergeEntryAdded`.
    ///
    /// [`TaskRunner::finished`]: letibot_tools::builtins::task::TaskRunner::finished
    fn enqueue_finished(&self, sub_id: &str) {
        use letibot_tools::builtins::task::Finished;
        match self.enqueue_entry(sub_id) {
            Ok(Some(entry)) => {
                // **The pane's row**, for every session — see the doc above for why this one is
                // broadcast and the sentence below is not. The whole entry travels, so a head
                // that missed the snapshot sees it rather than only later changes.
                self.registry
                    .broadcast(letibot_sessionlog::SessionEvent::MergeEntryAdded {
                        entry: crate::mergequeue::wire_entry(&entry),
                    });
                // **The person's sentence**, for this conversation only.
                if let Some(hub) = self.registry.get(&self.base.session_id) {
                    hub.publish(letibot_sessionlog::SessionEvent::Warning {
                        code: "merge_queued".into(),
                        detail: format!(
                            "`{}` is in the merge queue as `{}` — the gatekeeper reviews it \
                             against the brief the child was given, then the queue rebases it at \
                             the tip of main and runs the gate.",
                            entry.branch, entry.id
                        ),
                        compaction: None,
                    });
                }
            }
            // **The ordinary case for a plain `task`, and it says nothing.** A child that works
            // in the parent's own tree leaves no branch to land, and a line per such child would
            // be noise on every `task` call there is.
            Ok(None) => {}
            Err(why) => {
                if let Some(hub) = self.registry.get(&self.base.session_id) {
                    hub.publish(letibot_sessionlog::SessionEvent::Warning {
                        code: "merge_not_queued".into(),
                        detail: format!(
                            "the branch of `{sub_id}` was NOT enqueued: {why} It is still on \
                             its branch in its worktree, and nothing is lost — but nothing will \
                             land it until it is enqueued."
                        ),
                        compaction: None,
                    });
                }
            }
        }
    }

    /// **The write, and a store that will not open is a refusal rather than a silent drop.**
    ///
    /// The half of [`TaskRunner::finished`] that does the work, on its own so the two callers
    /// can have different shapes of the same answer: the trait method needs the tool layer's
    /// `Finished`, and the enqueue's own notice needs the row it wrote — for the
    /// `MergeEntryAdded` it broadcasts.
    ///
    /// `Ok(None)` is *there is no branch of its own to land*: a plain `task` child, a child of
    /// the main checkout, or a handle this session never placed. It is not a failure, and the
    /// caller says nothing about it.
    ///
    /// [`TaskRunner::finished`]: letibot_tools::builtins::task::TaskRunner::finished
    fn enqueue_entry(
        &self,
        handle: &str,
    ) -> Result<Option<letibot_tokencore::store::MergeEntry>, String> {
        let Some(child) = self
            .placed
            .lock()
            .expect("placed children")
            .iter()
            .find(|c| c.handle == handle)
            .cloned()
        else {
            return Ok(None);
        };
        let Some(path) = self.base.store.clone() else {
            return Err(format!(
                "this daemon has no `--store`, and the merge queue lives in the session \
                 store — so there is nowhere to write the entry for `{}`. The branch is still \
                 in `{}`, and nothing is lost; a daemon started with `--store` will serve it \
                 when the child is enqueued again.",
                child.placement.branch, child.placement.path
            ));
        };
        // **The row, built by the queue's own function** — so the enqueue and the daemon's
        // reads agree about what an entry is by construction rather than by two copies of the
        // same five assignments. The entry's `session_id` is the CHILD's, which is what the
        // column documents itself as holding: the subagent that finished the branch, not the
        // parent that noticed.
        let now_ms = (crate::config::now_ns() / 1_000_000) as u64;
        let Some(entry) = crate::mergequeue::entry_for_finished(
            handle,
            handle,
            &child.brief,
            &child.placement,
            now_ms,
        ) else {
            return Ok(None);
        };
        let mut cell = self.merge_store.lock().expect("merge store");
        if cell.is_none() {
            *cell = Some(
                Store::open(&path)
                    .map_err(|e| format!("the session store at {}: {e}", path.display()))?,
            );
        }
        let store = cell.as_ref().expect("just opened");
        store
            .put_merge_entry(&entry)
            .map_err(|e| format!("the entry for `{handle}` could not be written: {e}"))?;
        Ok(Some(entry))
    }

    fn run_to_completion(
        &self,
        sub_id: &str,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
        slot: &TaskSlot,
        progress: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        use letibot_tools::builtins::task::Placement;
        let role = spec.role.as_str();
        // **The downgrade is a union**: whatever this session was denied, its
        // children are denied, plus what this call asks. A downgraded session
        // cannot spawn a wider child by naming a wider role.
        let downgrade = self.base.downgrade.and(&spec.downgrade);
        // Placement: a VM is booted on a copy of this session's workspace by the
        // child's own `open` (see `firecode.rs`); a boot that fails is a spawn that
        // fails, named — never a subagent run on the host and called a VM.
        let placement = spec.placement;
        let _ = Placement::Host;
        // A subagent is a real session: mint its id and create its hub. **The id was already
        // minted by [`HarnessTaskRunner::start`]** — it is what that call returned — so what
        // arrives here is a handle, and it is unique because [`mint_sub_id`] made it so. The
        // nanosecond in it is what keeps a subagent minted after a daemon restart from
        // colliding with a persisted one (a bare counter resets, and `adopt` checks only the
        // live registry, so a collision would RESUME the old subagent instead of spawning a
        // new one); the ordinal the mint can append is what keeps two children of one
        // millisecond — or of one nanosecond — from sharing the handle `task_result`
        // collects by. The title is the subtask's first line, so a picker row says what the
        // subagent was for.
        let spawned = std::time::Instant::now();
        let sub_id = sub_id.to_string();
        let title = subagent_title(prompt);
        // The subagent seats the role it was asked for — any seat this build knows
        // — and coder when none was named, which is the `task` tool's own default.
        // A role this build does not know is refused by name rather than seated
        // as coder: a survey asked for as `researcher` and run as a coder would
        // be a subagent with more than it was meant to have.
        let seat = if role.is_empty() || role == "coder" {
            Seat::Coder
        } else {
            Seat::parse(role)?
        };
        let parent = self.base.session_id.clone();

        // Publish the subagent's state on the **parent's** hub, so a head attached to
        // the parent sees the spawn and finish without subscribing to the subagent's
        // own hub. Same three states the journal records, same prompt.
        //
        // **`task` is the whole subtask and `prompt` keeps its old two-meaning shape**
        // (title on the opening states, the answer's first line on the finish) so a head
        // older than the field sees exactly what it saw before. A new head reads the row
        // from `task` and the subtitle from `answer` — see `SessionEvent::Subagent`.
        // **THE CHILD'S OWN MODEL, when it was given one** — the operator's ask, 2026-10-05:
        // *"I want to be able to have subagents using different models. say you deepseek
        // should be able to run local model"*, and the other way: *"local qwen in main
        // session should be able to run cloud glm"*. Resolved through `models_choice`, the
        // same door `/models` uses, so a name a person can type is a name a child can run
        // on and a key the picker finds is a key the spawn finds. A refusal is the tool's
        // own sentence and the spawn does not happen — never a child quietly run on the
        // parent's model while the record says otherwise.
        // **EVERY spawn goes through the model door — the absent `model:` case included.**
        //
        // This resolved a model only when one was NAMED, and a spawn without one kept
        // `sub_cfg`'s inherited window and `ledger_scale`: the PARENT's, which for a session on
        // a provider is the cloud model's number, while what answers the child is the daemon's
        // own server. Three children died that way (see [`child_window_refusal`]). `local` is
        // not a guess about the absent case — it is what `models_choice` and `/models local`
        // already mean by it, and the child's `provider: None` below is that same door.
        // **The call's own word for the model, then the project file's for this seat, then
        // `local`.** The two leticode keys are read here and nowhere else: the call's `model:`
        // beats `[roles.<seat>] model`, which beats the file's `subagent_model`, which beats
        // `local` below. Read at the spawn rather than at daemon start because the seat is
        // only known now — and read at all because both keys were parsed, disclosed and
        // answered by nobody until this line, which is the defect the feature is written
        // against. The label the refusals below name is this one, so a child stopped for its
        // window says the model the PROJECT chose for it.
        let want = spec
            .model
            .as_deref()
            .or_else(|| self.base.leticode.spawn_model(seat.as_str()))
            .unwrap_or("local");
        // **And measure the local window when nobody remembered one.** `retune_window` fills it
        // *on the way out to a first provider*, so a session that STARTED on one has never
        // measured its own server — this session had not. `served_ctx` is the same `/props`
        // read the daemon's own start makes, so a `local` child is given the server's number
        // rather than `None`, which is no wall at all.
        let local_window = {
            let remembered = *self.local_window.lock().unwrap_or_else(|e| e.into_inner());
            match remembered {
                Some(w) => Some(w),
                None => Some(letibot_turn::serving::served_ctx(&self.base.endpoint)),
            }
        };
        let (sub_model, sub_provider, sub_window, sub_local) =
            match subagent_model(want, local_window) {
                Ok(m) => (m.label, m.provider, Some(m.window), m.local),
                Err(why) => return Err(why.join("; ")),
            };
        // **A declared local model: check the weights, then measure the wall.**
        //
        // The child tokenizes with the parent's vocabulary — one daemon, one GGUF — so
        // the same refusal a session switch gets applies here, from the same function
        // rather than a second copy of the reasoning.
        //
        // And the window is this box's, not the daemon's. Measured on this fleet the
        // day it was written: the same 27B GGUF at `n_ctx` 262144 on `127.0.0.1:8080`
        // and 57344 on `192.168.1.78:8082`. A child seated on the smaller one while
        // planning against the larger would run off the end of the KV cache with every
        // check it makes saying there is room — which is why a block that states no
        // window is probed here rather than allowed to inherit.
        let (sub_endpoint, sub_window) = match &sub_local {
            Some(m) => {
                let want_at =
                    parse_local_url(&m.url).map_err(|e| format!("[model.\"{}\"] {e}", m.name))?;
                if let Some(why) = local_model_vocab_refusal(&want_at, m, &self.base.vocab_gguf) {
                    return Err(why);
                }
                let w = match sub_window {
                    Some(Some(w)) => Some(Some(w)),
                    _ => Some(letibot_turn::serving::served_ctx(&want_at)),
                };
                (Some(want_at), w)
            }
            None => (None, sub_window),
        };
        // **And a window nobody can name may still be the parent's own — if the child runs on
        // the parent's own MODEL.** Same model, same window: that is the one case where the
        // parent's number is not a guess about a different server, and it is the case the
        // catalogue cannot answer (an explicit `--context-window` on an uncatalogued model).
        // A child on a DIFFERENT model never inherits it, which is the defect being fixed.
        let sub_window = match sub_window {
            Some(Some(w)) => Some(Some(w)),
            _ if runs_on_the_parents_model(&sub_provider, &self.base.provider) => {
                Some(self.base.context_window)
            }
            _ => sub_window,
        };
        if let Some(why) = child_window_refusal(want, sub_window) {
            return Err(why);
        }
        let task = prompt.to_string();
        let publish = |state: &str, said: &str, answer: Option<String>| {
            if let Some(hub) = self.registry.get(&parent) {
                hub.publish(SessionEvent::Subagent {
                    subagent_id: sub_id.clone(),
                    state: state.to_string(),
                    prompt: said.to_string(),
                    role: seat.as_str().to_string(),
                    task: task.clone(),
                    model: sub_model.clone(),
                    answer,
                });
            }
        };

        // **`opening`, not `running`, until the child is actually open.** A spawn
        // that said "running" from this line was listed in the subagents pane
        // while its VM was still copying the workspace — for the operator, a row
        // they could press Enter on that led to an empty conversation (measured
        // 2026-09-16). `opening` is a row that says what is happening and that
        // there is nothing to attach to yet; `running` is published below, after
        // the harness is open and the hub is in the registry.
        self.tasks.record(crate::tasks::TaskEntry {
            name: sub_id.clone(),
            role: seat.as_str().to_string(),
            state: "opening".into(),
            tokens: 0,
            elapsed: 0.0,
            prompt: title.clone(),
            parent: parent.clone(),
        });
        publish("opening", &title, None);
        // **And say which wall the child plans against**, in the line a person is already
        // reading. `config.rs` records the other half of this bug in its own words — *"the
        // silent half of this bug was that nobody could tell which window was in force"* —
        // and for a child, until now, NOTHING recorded it: not the store, not the log, not
        // this line. It is also what would have made the three dead children legible in one
        // glance instead of after a query against the store.
        let on_window = match sub_window {
            Some(Some(w)) => format!(" on {sub_model} ({w} token window)"),
            _ => format!(" on {sub_model}"),
        };
        progress(&match placement {
            Placement::Firecode => format!(
                "opening subagent {} in a firecode VM: copying the workspace, booting{on_window}",
                letibot_sessionlog::registry::short_id(&sub_id)
            ),
            _ => format!(
                "opening subagent {}{on_window}",
                letibot_sessionlog::registry::short_id(&sub_id)
            ),
        });
        // **Every early return from here on records the failure rather than leaving a
        // "running" row forever** — and it settles the child's slot as it does, because this
        // function no longer returns to a caller that settles for it (see [`AnswerOnce`]).
        let answer = AnswerOnce::new(slot);
        let fail = |why: String| {
            answer.say(letibot_tools::builtins::task::TaskStatus::Failed { why: why.clone() });
            self.tasks.record(crate::tasks::TaskEntry {
                name: sub_id.clone(),
                role: seat.as_str().to_string(),
                state: "failed".into(),
                tokens: 0,
                elapsed: spawned.elapsed().as_secs_f64(),
                prompt: why.clone(),
                parent: parent.clone(),
            });
            publish("failed", &why, None);
            why
        };

        #[allow(unused_mut)]
        let mut wiring = letibot_sessionlog::registry::SessionWiring {
            // **The child's own model when it has one**, so its session row and the
            // picker name what actually answers it; the parent's otherwise.
            model: if sub_model.is_empty() {
                self.base.model.clone()
            } else {
                sub_model.clone()
            },
            dialect: self.base.dialect.name().to_string(),
            // **The box that will answer this child**, which is not the daemon's own
            // when the child was seated on a declared local model. The row is what the
            // picker and the store show, and a row naming the wrong address is the
            // same silent-provenance defect as a model name that does not match.
            endpoint: sub_endpoint
                .as_ref()
                .map(|e| e.authority())
                .unwrap_or_else(|| self.base.endpoint.authority()),
            workspace: self.base.workspace.display().to_string(),
        };
        // Built, not registered: nothing can switch into it, and the worker is not
        // told to open it, until `open_with_registry` below has succeeded.
        let sub_hub = self.registry.new_hub(sub_id.clone());

        let point = self.point();
        let mut sub_cfg = Config {
            session_id: sub_id.clone(),
            title: title.clone(),
            seat,
            parent_session_id: Some(parent.clone()),
            // **The parent's point, read LIVE and not off `base`.** `base` is the config as it
            // stood at the parent's open, and nothing updates it — so a child built from it came
            // up at the point its parent had *opened* at, which is the operator's measured
            // defect of 2026-10-06: session at `allow-all` (consented), children at
            // `automode-edits`, every child's `bash` asking a person a question the parent had
            // already answered. What travels is the point the session is at; whether this child's
            // SEAT can carry it is the child's own question, answered at its open by
            // `Mode::inherited_by`, which clamps and says so rather than refusing to open.
            mode: point,
            // **One level down, incremented once and only here** (R58). It rides the
            // config the child is built from, the way `unconfined` and the workspace do,
            // so a grandchild's `depth` is 2 without anything walking a parent chain —
            // and `HarnessTaskRunner::start` refuses the spawn at the cap before this is
            // reached, so a config at the cap never gets a child to increment for.
            depth: self.base.depth + 1,
            downgrade,
            placement,
            // **The child's own model, when it has one.** `provider` is the whole switch:
            // `None` is the daemon's own server, exactly as `/models local` means, and a
            // `ProviderConfig` is a cloud preset whose key `models_choice` already checked.
            provider: sub_provider,
            ..self.base.clone()
        };
        // **The worktree `task_start` arranged is the child's workspace**, not the
        // parent's tree. The placement rides the spec (see `TaskSpec::worktree`), and
        // a child whose workspace is not the worktree the tool just reported is the
        // same silent provenance defect as a model name that does not match — the
        // child would be working in the parent's tree while the answer said it was in
        // its own. `None` is a plain `task`, which keeps the parent's workspace.
        if let Some(wt) = &spec.worktree {
            sub_cfg.workspace = std::path::PathBuf::from(&wt.path);
        }
        // **And the local model's own address, alias and sampling**, after the spread,
        // because `..self.base.clone()` would otherwise put the parent's back.
        if let (Some(e), Some(m)) = (&sub_endpoint, &sub_local) {
            sub_cfg.endpoint = e.clone();
            sub_cfg.model = m.model.clone();
            if !m.profile.sampling.is_empty() {
                sub_cfg.sampling = serde_json::Value::Object(m.profile.sampling.clone());
            }
            if let Some(eff) = &m.profile.effort {
                sub_cfg.effort = Some(eff.clone());
            }
        }
        if let Some(w) = sub_window {
            sub_cfg.context_window = w;
            // The child's ledger is its own conversation, so it has no measured ratio:
            // the provider's number stands rather than the parent's scaled one.
            sub_cfg.ledger_scale = None;
        }
        // **And the project's parameters for the role this child was seated on**, which are
        // the last word on how it samples: the file beats the `[model.*]` profile applied
        // just above, the way the project beats `~/.config/letibot/` everywhere else. They
        // touch `sampling` and nothing else — least of all the window set one statement
        // above, which belongs to the box that answers this child.
        apply_spawn_parameters(&mut sub_cfg, &self.base.leticode, seat.as_str());

        // Reassemble the shared parts so the sub harness can borrow them for the
        // duration of this call. Cheap: the vocabs and the wiring are already `Arc`.
        let parts = Parts {
            vocab: self.vocab.clone(),
            wiring: self.wiring.clone(),
            mode_store: self.mode_store.clone(),
            tasks: self.tasks.clone(),
            lsp: self.lsp.clone(),
            skills: self.skills.clone(),
            // **The child joins this tree's watcher set** (R58): the parent's own set, which
            // carries the tree's ROOT (for a permission card) and the level above (for what this
            // session cannot drain, and for the ring that says so). The parent is a root at depth
            // 0, and at depth ≥ 1 the parent's set already carries the same root from the same
            // field one level up, so the card reaches the head with no parent-chain walk.
            tree_watch: self.tree_watch.lock().expect("tree watch").clone(),
            // **And the child shares this tree's handle list** (R58), the other half of
            // *"every ancestor's handle can be collected from any level"*: a grandchild
            // pushed into this list is collectable and killable by the root, which is the
            // session the tree's wake is delivered to.
            tree_slots: Some(self.slots.clone()),
            // **And the tree's head answers** (R58's ask route). A child has no head, so what
            // reaches an `ask` is posted to the tree's ROOT — and the root's `Answers` is
            // what settles it. Handed down through `Parts` exactly as the watcher set and
            // the handle list are, so a grandchild reaches the same root with no
            // parent-chain walk. `None` is a tree with no head-wired adjudicator, and the
            // child's adjudicator then refuses by name instead of posting a card nothing
            // could answer.
            tree_head: self
                .head_answers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        };

        // **The child's adjudicator: its asks surface at the tree's ROOT.** The operator's
        // ruling, and the whole of what changed here — a child used to deny every `ask` on a
        // policy (`a subagent has no operator to ask`), and now the card goes to the session
        // whose head exists and the operator's answer comes back as the child's decision.
        // The addressing itself is [`subagent_ask_target`], which says why the root comes
        // from the watcher set rather than from the parent chain.
        let (sub_tree_root, sub_head) = subagent_ask_target(
            &self.registry,
            parts.tree_watch.as_deref(),
            &parent,
            parts.tree_head.as_ref(),
        );
        let sub_adjudicator = crate::answers::SubagentAdjudicator::new(
            letibot_sessionlog::event::SubagentAsk {
                handle: sub_id.clone(),
                // The task's first line — the same title the picker row and the parent's
                // `Subagent` event carry, so the card names the child the way every other
                // surface does.
                task: title.clone(),
                root: sub_tree_root.clone(),
            },
            sub_head,
        );

        let mut sub = Harness::open_with_registry(
            &parts,
            sub_cfg,
            sub_hub.clone(),
            Some(Box::new(sub_adjudicator)),
            None,
            self.registry.clone(),
        )
        .map_err(|e| fail(e.to_string()))?;
        // Open. Now it is a session a head can switch into, and now it is running.
        //
        // **Adopted into the REGISTRY and not into `Sessions::open`, and that difference is R58's
        // whole constraint — and NOT a hole any more.** A head can peek at this session and attach
        // to it (both go through `registry.resolve`), while the daemon holds no `Harness` for it:
        // `Sessions::wake` cannot run its turn. What that used to mean is that a settlement rung
        // for a session created HERE was discarded, which made depth 2 a hole — the child's own
        // watcher rings the child, and nobody answered. **The daemon serves it by handing the wake
        // to the thread that owns it** (`Hub::wake_its_own_reader`, answered by `serve_child`), so
        // the ring names the session that owns the settlement and the delivery is real at every
        // depth. See `Sessions::wake`.
        self.registry
            // **Cloned, because the serving loop below needs it back**: this hub is the child's
            // queue, and `serve_child` blocks on it until the daemon closes it — so the same
            // `Arc` the registry was given is the one the child serves from.
            .adopt(sub_hub.clone(), title.clone(), wiring, Some(parent.clone()))
            // **A taken id here is a MINTING bug, and it says so.** `mint_sub_id` is supposed
            // to make this unreachable; if it is reached, the sentence has to name the id and
            // say what it costs, because the quiet symptom is a child's whole reply that
            // cannot be collected — `task_result` finds the first slot under a handle, so the
            // second child under one id is unaddressable (leticl, 2026-10-04). Reported as
            // what it is rather than as the child failing, which would look like the child's
            // own fault.
            .map_err(|e| {
                fail(format!(
                    "the id `{sub_id}` was already taken ({e}). That is a MINTING bug, not \
                     this child failing: the id is the handle `task_result` collects by, so a \
                     duplicate makes a reply unreachable. The mint has to be made unique — see \
                     `mint_sub_id` — rather than the reply written off."
                ))
            })?;
        self.tasks.record(crate::tasks::TaskEntry {
            name: sub_id.clone(),
            role: seat.as_str().to_string(),
            state: "running".into(),
            tokens: 0,
            elapsed: spawned.elapsed().as_secs_f64(),
            prompt: title.clone(),
            parent: parent.clone(),
        });
        publish("running", &title, None);
        progress(&format!(
            "subagent {} open after {:.1}s — running; ctrl-g lists it, enter attaches",
            letibot_sessionlog::registry::short_id(&sub_id),
            spawned.elapsed().as_secs_f64()
        ));
        // **And if it is NOT at this session's point, say so where the person is looking.**
        //
        // The child's own settings row carries the same sentence (`child_mode_source`), and a
        // row is read by whoever opens that child's pane. This line is the half the parent's
        // reader gets: the person watching a tree of children is the one who has to know that
        // one of them is asking about things its parent already answered — or that one of them
        // cannot exec, and why.
        if sub.config().mode.name != point.name {
            progress(&format!(
                "subagent {} opened at `{}`, not this session's `{}` — {}",
                letibot_sessionlog::registry::short_id(&sub_id),
                sub.config().mode.name,
                point.name,
                sub.mode_source()
            ));
        }

        // **Through the seam every session has — the child's whole defect was that
        // this call was a bare `submit`.** The operator's requirement, in their
        // words: *"subagent is a normal session. it must be able to compact on tool
        // call overrun"*. A bare `submit` returns `ContextWall` as an error and
        // nothing behind this line reacts to it — the child's task failed with a
        // notice promising a compaction nothing attempted (measured 2026-10-06:
        // 207 rounds, 1,131,717 of 1,146,137 tokens, one transcript, no fork).
        // [`Harness::submit_as_a_normal_session`] compacts at the wall and
        // continues on the summary, bounded; a wall it cannot dig out of still
        // comes back here as the honest failure it is.
        let reply = sub
            .submit_as_a_normal_session(prompt)
            .map_err(|e| fail(e.to_string()))?;
        let tokens: u64 = reply.metrics.iter().map(|m| m.predicted_tokens).sum();
        let first_line = reply.text.lines().next().unwrap_or("").to_string();
        self.tasks.record(crate::tasks::TaskEntry {
            name: sub_id.clone(),
            role: seat.as_str().to_string(),
            state: "done".into(),
            tokens,
            elapsed: spawned.elapsed().as_secs_f64(),
            prompt: first_line.clone(),
            parent: parent.clone(),
        });
        publish("done", &first_line, Some(first_line.clone()));
        // **A VM's life is its turn's, and closing it is where the placement sentence comes
        // from** — the one the parent reads as *where the work went*. So a VM child answers the
        // task and no more: its substrate cannot be held open for a later question, and that is
        // stated here rather than discovered as silence.
        if placement != letibot_tools::builtins::task::Placement::Host {
            let landed = sub.close_backend();
            let said = match landed {
                Some(where_) => format!("{}\n\n[subagent placement] {where_}", reply.text),
                None => reply.text,
            };
            answer.say(letibot_tools::builtins::task::TaskStatus::Done {
                answer: said.clone(),
            });
            // **The child has finished, so its branch goes to the queue** — the runner's own
            // half of `task_start`'s door, taken here because THIS is the finish. See
            // [`HarnessTaskRunner::finished`]: the two notices (this one and a `task_result`
            // that arrives later) are one entry under one id.
            self.enqueue_finished(&sub_id);
            return Ok(said);
        }
        // **A host child stays reachable** — the operator's ruling (*"mid turn, post turn
        // whatever"*), and this is where the answer is given: **as the turn ends**, not on the
        // way out of this function, because what follows does not end until the hub does. The
        // caller used to settle from this function's return, which a park would have delayed for
        // the life of the daemon — `task_result` would simply never answer.
        answer.say(letibot_tools::builtins::task::TaskStatus::Done {
            answer: reply.text.clone(),
        });
        // **The child has finished, so its branch goes to the queue.** Here rather than at the
        // thread's exit, and the difference is the whole reason `AnswerOnce` exists: a host
        // child parks in `serve_child` below and does not leave this function until the daemon
        // closes its hub, so enqueueing on the way out would hold a finished branch out of the
        // queue for the life of the daemon. The answer being given IS the finish.
        self.enqueue_finished(&sub_id);
        serve_child(&mut sub, &sub_hub, &sub_id);
        // The hub closed, so the daemon is going away. Released here rather than when the harness
        // is dropped, for the reason it always was — and there is no placement sentence to add on
        // the host, which `close_backend` says by returning `None`.
        let landed = sub.close_backend();
        Ok(match landed {
            Some(where_) => format!("{}\n\n[subagent placement] {where_}", reply.text),
            None => reply.text,
        })
    }
}

/// **The intent diff at a turn boundary**, or `None` when there is nothing to say
/// — which is the common case and is meant to be.
///
/// A free function because the choice it makes is the one worth testing on its own,
/// and testing it through a `Harness` would need a model server to produce a turn.
///
/// Two functions, and the difference is `prose`:
///
/// * [`letibot_tools::builtins::intent::close_the_turn`] reads the assistant's own
///   text for the `commitments` heuristic **as well as** the tool-declared half.
/// * `IntentLedger::reconcile(turn_id, "")` is the tool-declared half alone, which
///   that method's own doc calls *"the deterministic one"*.
///
/// The deterministic half is the default because of the rule this whole strand is
/// under: **nothing widens by default.** The prose heuristic can only fire on a turn
/// that ran nothing at all, which for a plain answer is the normal case, so switching
/// it on for every existing session would put *"you said you would X"* into
/// conversations that were working.
///
/// The deterministic half needs no flag because it cannot fire unless the model used
/// `todo` or `goal`, and those are seated only by a role that names them. A default
/// `letibot` session is behaviourally identical with it on.
fn steer_for_turn(
    ledger: &IntentLedger,
    prose: bool,
    turn_id: &str,
    items: &[TranscriptItem],
) -> Option<String> {
    if prose {
        intent_tools::close_the_turn(ledger, turn_id, items)
    } else {
        ledger.reconcile(turn_id, "").steering()
    }
}

/// **Do the weights at this address match the vocabulary we tokenize with?** — the
/// refusal, or `None` to go ahead.
///
/// A free function because two callers need the identical answer and must not drift:
/// [`Harness::set_local_model`] for a session switching, and the spawn path for a
/// child seated on a declared local model. A second copy of this check would be a
/// second answer to *are these the same weights*, which is the class of defect this
/// tree keeps finding by looking for it.
///
/// # Why it exists at all
///
/// The engine tokenizes HERE, with this daemon's GGUF, and sends token ids. Point it
/// at a server holding different weights and nothing fails: the ids are valid numbers
/// that mean other words. There is no error to catch and no output that looks wrong
/// until a person reads the transcript, which makes it the worst shape of bug this
/// code can produce — so it is refused up front rather than detected later.
///
/// Three outcomes, each a sentence:
///
///   * `/props` names the same GGUF file — `None`, go ahead.
///   * it names a different one — refused, with both names printed.
///   * nothing answers, or it names no model (a proxy, a server without `/props`) —
///     refused, naming the key that asserts it by hand. An operator writing
///     `same_vocab = true` is a decision on the record; this assuming it would not be.
fn local_model_vocab_refusal(
    want: &Endpoint,
    m: &letibot_provider::keys::LocalModel,
    vocab: &std::path::Path,
) -> Option<String> {
    // `same_vocab` is not sampling, so the reader files it under `unknown` — which is
    // where a key it does not interpret belongs. Read here, where it means something.
    let asserted = m
        .profile
        .unknown
        .iter()
        .any(|u| u == "same_vocab" || u.starts_with("same_vocab "));
    if asserted {
        return None;
    }
    let mine = vocab_basename(vocab);
    match letibot_turn::serving::served_model(want) {
        Ok(theirs) => {
            let theirs_base = vocab_basename(std::path::Path::new(&theirs));
            (theirs_base != mine).then(|| {
                format!(
                    "{} serves `{theirs}` and this daemon tokenizes with `{}`. The ids are \
                     computed here and sent as numbers, so pointing them at other weights is \
                     silent corruption rather than an error. Start a daemon on those weights, \
                     or put `same_vocab = true` in [model.\"{}\"] to say you know they match.",
                    want.authority(),
                    vocab.display(),
                    m.name
                )
            })
        }
        Err(why) => Some(format!(
            "{} did not answer /props with a model ({why}), so this cannot check that its \
             weights are the ones `{}` tokenizes for. Put `same_vocab = true` in \
             [model.\"{}\"] to assert it.",
            want.authority(),
            vocab.display(),
            m.name
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
fn vocab_basename(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Sampling as one short line for a reply the operator reads, in the file's own order.
fn render_sampling(s: &serde_json::Map<String, serde_json::Value>) -> String {
    s.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A provider backend from its config: the preset, the key (a missing one is a
/// refusal naming the variable and the file), the model, the switches.
pub fn build_provider(
    pc: &crate::config::ProviderConfig,
    sampling: &serde_json::Value,
) -> Result<Box<dyn letibot_backend::MessagesBackend>, String> {
    let preset = letibot_provider::Preset::parse(&pc.name)?;
    let creds = letibot_provider::keys::resolve(preset, pc.api_key.as_deref(), None)
        .map_err(|e| format!("provider {}: {e}", pc.name))?;
    let mut p = letibot_provider::OpenAiProvider::new(preset, pc.model.as_deref(), creds);
    p.thinking = pc.thinking;
    p.sampling = provider_sampling(sampling);
    Ok(Box::new(p))
}

/// The sampling the operator configured, as a provider's body fields. The local
/// server's knobs (`top_k`, `seed`, llama.cpp's own names) are not the API's;
/// only what the OpenAI shape carries goes through.
fn provider_sampling(sampling: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Some(obj) = sampling.as_object() {
        for k in [
            "temperature",
            "top_p",
            "seed",
            "max_tokens",
            "presence_penalty",
            "frequency_penalty",
        ] {
            if let Some(v) = obj.get(k) {
                out.insert(k.to_string(), v.clone());
            }
        }
    }
    serde_json::Value::Object(out)
}

/// The parent's ruleset plus a `deny` per tool of a denied class. The tools are
/// not seated either (see `without_access`), so this is the third reader of the
/// same fact and the one that holds if a name reaches the gate anyway: last
/// rule wins in opencode's `evaluate`, and these are last.
///
/// Two sources of names. The seated schemas, by their declared class — after
/// `without_access` that set is empty, which is the point. And the well-known
/// names of each class whether seated or not, so a tool registered outside the
/// role later (an `extra_tool`) that carries a denied class is refused by name
/// rather than admitted because nobody wrote a rule for it. A rule about an
/// unseated name costs nothing.
fn downgraded_ruleset(
    parent: &letibot_tools::permission::Ruleset,
    downgrade: &letibot_tools::schema::Downgrade,
    seated: &[letibot_tools::schema::ToolSchema],
) -> letibot_tools::permission::Ruleset {
    use letibot_tools::permission::{Action, Rule};
    use letibot_tools::schema::Access;
    let mut rules = parent.clone();
    if downgrade.is_none() {
        return rules;
    }
    let well_known: &[(Access, &[&str])] = &[
        (Access::Write, &["write", "edit", "exit_plan_mode"]),
        (Access::Exec, &["bash", "monitor", "job_kill", "lsp"]),
        (
            Access::Network,
            &["flowy", "web_search", "web_fetch", "forge", "mcp"],
        ),
    ];
    let mut names: Vec<String> = Vec::new();
    for (class, known) in well_known {
        if downgrade.denies(*class) {
            names.extend(known.iter().map(|n| n.to_string()));
        }
    }
    names.extend(
        seated
            .iter()
            .filter(|s| downgrade.denies(s.access))
            .map(|s| s.name.clone()),
    );
    names.sort();
    names.dedup();
    for name in names {
        rules.push(Rule::new(&name, "*", Action::Deny));
    }
    rules
}

/// The role a seat resolves to, with `bash` removed when it was not asked for.
///
/// The removal is here rather than in `letibot_tools::roles` because it is a
/// *session* decision and not a change to what the runner role is: `m2_runner` is
/// nine tools and says so, and a session that took eight of them is a session, not
/// a different role. `Registry::resolve_role` refuses a role naming a tool the
/// build does not have, so dropping the name is the only way to seat a runner
/// without a shell — leaving it in and hoping the tool refuses would put `bash` in
/// the prompt, which is the model being told it has a capability it does not.
fn role_for(cfg: &Config) -> Role {
    role_for_seat(cfg.seat, cfg)
}

/// The same table, reached from a seat the store named rather than from `cfg`.
///
/// `cfg` is still needed for [`Config::allow_bash`], which is a daemon-wide flag and
/// deliberately not per session: a shell is the one capability that is not the
/// session's to record.
fn role_for_seat(seat: Seat, cfg: &Config) -> Role {
    let mut r = base_role_for_seat(seat, cfg);
    // **The room.** The `flowy` tool is registered by `Sessions` for every root
    // session, and the role names it under exactly the same condition —
    // `Registry::resolve_role` refuses a role naming a tool the session does not
    // have. The runner has no spare seat (`m2_runner` is nine and says so).
    // Always a door for a root session, seat or no seat: without one the tool
    // says so and names `/flowy login`. That is what lets a seat arrive while
    // the session is open. A subagent hears through its parent.
    if cfg.parent_session_id.is_none() && seat != Seat::Runner {
        r.tools.push("flowy".into());
    }
    // **`web_search` is seated only when something is behind it**, and a subagent
    // inherits that, because a survey is the errand most worth handing one.
    //
    // Conditional for the reason `external/mod.rs` gives: a tool schema is prompt
    // bytes in the stable prefix, so seating it unconditionally would re-prefill
    // every stored conversation on this box to add a tool that refuses. A session
    // started without `--web-search` is byte-identical to yesterday's.
    if cfg.web_search.is_some() && seat != Seat::Runner {
        r.tools.push("web_search".into());
        r.max_tools += 1;
    }
    // **`web_fetch` seats on the same rule as `web_search`**: only when
    // something is behind it, so a session started without `--web-fetch` is
    // byte-identical to one from before the flag existed.
    if cfg.web_fetch && seat != Seat::Runner {
        r.tools.push("web_fetch".into());
        r.max_tools += 1;
    }
    r
}

fn base_role_for_seat(seat: Seat, cfg: &Config) -> Role {
    match seat {
        Seat::Orchestrator => roles::m1_orchestrator(),
        Seat::Planner => roles::planner(),
        Seat::Researcher => roles::m3_researcher(),
        Seat::Coder => {
            let mut r = roles::m2_coder();
            // Same rule as the runner below: the role lists `bash`, the flag seats it.
            if !cfg.allow_bash {
                r.tools.retain(|t| t != "bash");
            }
            r
        }
        Seat::Runner => {
            let mut r = roles::m2_runner();
            if !cfg.allow_bash {
                r.tools.retain(|t| t != "bash");
            }
            r
        }
        Seat::Leticode => {
            let mut r = roles::leticode();
            if !cfg.allow_bash {
                // The whole exec surface is behind the flag: `bash` and the job
                // verbs it feeds, and `monitor` which watches processes a shell
                // starts. A session with no shell has nothing to wait on, read,
                // kill or watch, and seating those would be a capability claim the
                // backend refuses.
                r.tools.retain(|t| {
                    !matches!(
                        t.as_str(),
                        "bash"
                            | "job_list"
                            | "job_output"
                            | "job_wait"
                            | "job_kill"
                            | "monitor"
                            | "pkill"
                            | "ps"
                    )
                });
            }
            r
        }
    }
}

/// **Where layer A is standing**, and what this daemon can honestly claim about it.
///
/// `docs/boundary-and-adjudication.md` §5 states the requirement rather than
/// assuming it, *"because a requirement crosses a merge where an assumption does
/// not"*: `Surroundings::with_pinned_shell` is honest only where the spawn path
/// `env_clear()`s before its explicit pairs so `PATH` is pinned rather than
/// inherited, and unsets `BASH_ENV`, `ENV`, `SHELLOPTS` and `BASHOPTS` —
/// `BASH_ENV` **is** sourced by bash for non-interactive shells, so a
/// distribution where `/bin/sh` is bash has a real injection point.
///
/// Since R10 the host spawn does exactly that — `HostProcesses` pins the `PATH`
/// it was constructed with, clears the rest, filters the four by name, and the
/// test `a_bash_env_planted_in_the_parent_never_reaches_the_child` holds the
/// claim — so a seat with an exec backend declares the pin. A seat without one
/// spawns no shell at all, so there is nothing to claim and `Unknown` stays,
/// under which a **bare** command name is unresolved and the call is `not_run` —
/// the fail-closed direction. A resolved parse is not a resolved meaning, and a
/// shell resolves a bare name through aliases, functions and `PATH`, none of
/// which are in the text.
///
/// What is filled in either way is the part this daemon **does** know: the
/// workspace root and `$HOME`, both of which layer A needs to place a path.
/// [`letibot_tools::Surroundings::from_env`] is the constructor that says
/// reading the environment is a decision at a call site.
/// **Where this daemon's sessions put working artifacts.**
///
/// One function so the backend that hands it to tools and the gate that decides
/// about paths inside it cannot disagree about where it is — a gate pointed at a
/// different directory than the one the tools use is a gate that permits deletion
/// in a place nothing writes, and asks about the place everything does.
///
/// Per daemon process rather than per session: a pid is stable for the daemon's
/// life, which is what "this box is running letibot right now" means, and a
/// session id in the path would make the directory unfindable from the one place
/// that has to clean it up.
pub fn scratch_dir() -> std::path::PathBuf {
    // **THE HEAD'S OWN RULE, APPLIED TO THE DAEMON'S HALF.** This used to be
    // `/tmp/letibot-scratch-<pid>` — `temp_dir()` at a predictable name, world-readable
    // — and it holds what the jobs write, the web pages fetch spills, the peek logs:
    // anything the model read. The TUI's `head_runtime_dir` already ruled that shape
    // wrong for its own spills ("a world-readable file at a name anyone can predict is
    // both a disclosure and the classic symlink target"), and the operator's ask,
    // 2026-10-05, was the same ruling one layer down: *"job output should go to your
    // scratch directory."* `$XDG_RUNTIME_DIR` is per-user, mode 0700 and tmpfs by
    // construction; the fallback creates its per-user directory at 0700 rather than
    // inheriting `/tmp`'s 0755 (the creation site sets the mode — `create_dir_all`
    // cannot).
    //
    // Still per daemon process, and still no session id in the path, for the reason
    // above: findable from the one place that has to clean it up.
    let pid = std::process::id();
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d)
            .join("letibot")
            .join(format!("scratch-{pid}")),
        None => std::env::temp_dir()
            .join(format!("letibot-{}", unsafe { libc::getuid() }))
            .join(format!("scratch-{pid}")),
    }
}

pub fn surroundings_for(cfg: &Config) -> letibot_tools::Surroundings {
    let mut env = letibot_tools::Surroundings::from_env(cfg.workspace.display().to_string())
        .with_known_hosts();
    // The gate places a path by region, and the scratch is its own region — see
    // `Region::Scratch`. Without this it reads as shared `/tmp`, and destruction
    // there is an ask.
    env.scratch = Some(scratch_dir().display().to_string());
    // The shell is pinned whenever the backend can start a process. Both the
    // confined backend (coder/runner) and the unconfined leticode one (`--bash`)
    // spawn through `HostProcesses`, which `env_clear`s and fixes `PATH` — so a
    // bare command name resolves through a pinned PATH either way, and the
    // `Unknown` that refused `echo` was the wrong claim for an unconfined seat.
    let may_exec = !cfg.downgrade.denies(letibot_tools::schema::Access::Exec);
    if (cfg.seat.needs_exec_backend() || (cfg.unconfined && cfg.allow_bash)) && may_exec {
        env.with_pinned_shell(
            "the exec backend spawns /bin/sh -c, non-interactive, with the \
             environment cleared before its own pairs; PATH is the one fixed at \
             seat time and BASH_ENV, ENV, SHELLOPTS, BASHOPTS are never set",
        )
    } else {
        env
    }
}

fn build_spiller(cfg: &Config) -> Result<letibot_tools::Spiller, HarnessError> {
    use letibot_tools::{FixedBudget, MemoryStore, NoBudget, Spiller};
    let budget: Box<dyn letibot_tools::InlineBudget> = match cfg.spill {
        // D6: unset is a genuine no-op, not a hidden constant. The daemon says so
        // at startup (`Config::disclosures`) so that "nothing spilled" is read as
        // "nothing was configured to" rather than as "nothing was big enough".
        SpillPolicy::Unset => Box::new(NoBudget),
        SpillPolicy::Inline(n) => Box::new(FixedBudget(n)),
    };
    let store: Box<dyn letibot_tools::SpillStore> = match &cfg.spill_storage {
        SpillStorage::Memory => Box::new(MemoryStore::new()),
        SpillStorage::Dir(d) => Box::new(
            letibot_tools::spill::FileStore::new(d, &cfg.session_id)
                .map_err(|e| HarnessError::Setup(format!("spill directory {d:?}: {e}")))?,
        ),
    };
    Ok(Spiller::new(budget, store))
}

#[cfg(test)]
mod subagent_model_tests {

    /// **A declared local model seats a child** — the operator's ruling, 2026-10-05:
    /// *"models from providers.toml must be usable for subagents"*.
    ///
    /// `provider` stays `None` because this IS the local path; what travels is `local`,
    /// which carries the address the child's config is pointed at. The two cannot be
    /// collapsed: `None` alone would mean the daemon's own server, which is a different
    /// box with a different window.
    #[test]
    fn a_declared_local_model_can_seat_a_subagent() {
        let f = providers(
            "a_declared_local_model_seats",
            "[model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\n\
             model = \"qwen-3.8-27b\"\nwindow = 57344\ntemperature = 0.7\n",
        );
        let m = super::subagent_model_in(Some(&f), "dense78", Some(Some(262_144)))
            .expect("a declared local model needs no key");
        assert_eq!(m.label, "dense78");
        assert!(m.provider.is_none(), "local, not metered");
        let local = m.local.expect("the declared model travels with the choice");
        assert_eq!(local.model, "qwen-3.8-27b");
        assert_eq!(local.url, "http://192.168.1.78:8082");
        assert_eq!(
            m.window,
            Some(57_344),
            "the BLOCK's window, never the parent's 262144 -- the same GGUF is served at \
             both sizes on this fleet and inheriting would run the child off the KV cache"
        );
    }

    /// A block that states no window leaves it `None` here, for the spawn path to
    /// measure off that server. Asserted because the alternative anybody would reach
    /// for -- inheriting the parent's -- is the bug.
    #[test]
    fn a_local_model_without_a_stated_window_does_not_inherit_the_parents() {
        let f = providers(
            "a_local_model_without_a_window",
            "[model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\nmodel = \"qwen-3.8-27b\"\n",
        );
        let m =
            super::subagent_model_in(Some(&f), "dense78", Some(Some(262_144))).expect("resolving");
        assert_eq!(
            m.window, None,
            "nobody has asked that server yet, and the parent's number is about another box"
        );
    }

    /// `window = ` is read, and a value that is not a token count is named rather than
    /// quietly leaving the child with no wall.
    #[test]
    fn a_stated_window_that_is_not_a_number_is_named() {
        let f = providers(
            "a_window_that_is_not_a_number",
            "[model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\nwindow = \"big\"\n",
        );
        let fleet = letibot_provider::keys::local_models(Some(&f));
        assert_eq!(fleet.len(), 1);
        assert_eq!(fleet[0].profile.window, None);
        assert!(
            fleet[0]
                .profile
                .unknown
                .iter()
                .any(|u| u.contains("window")),
            "{:?}",
            fleet[0].profile.unknown
        );
    }

    /// **The project's parameters reach the child, and its window does not come from them.**
    ///
    /// The operator: *"when used in subagents mode we need to call qwen with the parameters
    /// similar to dense78 - otherwise it overthinks terribly"* — and in the same breath
    /// *"we dont have the same context limit as with dense obviously"*. So the five samplers
    /// land on the config the child is built from, replacing what it would otherwise have
    /// inherited or taken from its model's `[model.*]` profile, and `context_window` — the
    /// wall [`child_window_refusal`] above is about — is not touched: a child's room is the
    /// number the box answering it reported.
    #[test]
    fn the_project_parameters_reach_the_child_and_the_window_is_not_taken_from_them() {
        // A child's config as the spawn builds it: the daemon's greedy literal in force and
        // the wall the server reported for the model this child actually runs on.
        let mut sub_cfg = Config::for_this_box("/tmp");
        sub_cfg.sampling = serde_json::json!({"temperature": 0.0, "top_k": 1, "seed": 7});
        sub_cfg.context_window = Some(57_344);

        let dir = std::env::temp_dir().join(format!("letibot-leticode-sub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(crate::leticode_config::FILE_NAME);
        std::fs::write(&path, "[roles.coder]\ntemperature = 1.0\n").expect("write");
        let leticode =
            crate::leticode_config::LeticodeConfig::load(&path).expect("a role's samplers");

        apply_spawn_parameters(&mut sub_cfg, &leticode, "coder");

        assert_eq!(
            sub_cfg.sampling["temperature"], 1.0,
            "the seat's own number"
        );
        assert_eq!(
            sub_cfg.sampling["top_p"], 0.8,
            "over the operator's dense78 set, key by key"
        );
        assert_eq!(sub_cfg.sampling["top_k"], 20.0);
        assert_eq!(
            sub_cfg.sampling["min_p"], 0.0,
            "and a min_p SET to zero is not absent"
        );
        assert_eq!(sub_cfg.sampling["presence_penalty"], 1.5);
        assert_eq!(
            sub_cfg.sampling["seed"],
            serde_json::Value::Null,
            "the daemon's greedy literal is replaced rather than merged under -- a profile that \
             named `temperature` must not keep `top_k = 1` from the built-in"
        );
        assert_eq!(
            sub_cfg.context_window,
            Some(57_344),
            "the wall is the box's, never dense78's: *\"we dont have the same context limit as \
             with dense obviously\"*"
        );

        // And the file has no way to say otherwise: a role table carrying a window is refused
        // where it is read, so there is no path by which a number from the profile could
        // reach the field asserted just above.
        std::fs::write(&path, "[roles.coder]\nwindow = 262144\n").expect("write");
        let why = crate::leticode_config::LeticodeConfig::load(&path)
            .expect_err("a window in a role's table is refused");
        assert!(why.contains("window"), "named: {why}");
    }

    fn providers(tag: &str, body: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-sub-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).expect("scratch");
        let f = d.join("providers.toml");
        std::fs::write(&f, body).expect("write");
        f
    }
    use super::*;

    /// **`local` is the daemon's own server and needs no key** — the deepseek→local half of
    /// the operator's ask, and the half that could never fail for a missing credential.
    #[test]
    fn local_resolves_to_no_provider_and_the_servers_own_window() {
        let m = subagent_model("local", Some(Some(262_144))).expect("local needs no key");
        assert_eq!(m.label, "local");
        assert!(m.provider.is_none(), "local is the daemon's own server");
        assert_eq!(
            m.window,
            Some(262_144),
            "the child on the local server plans against the SERVER's window, not the \
             cloud model's the parent was carrying"
        );
        // A session that never switched away has no remembered server window, and `None`
        // is *nobody measured one* rather than the parent's cloud number.
        let unknown = subagent_model("local", None).expect("still local");
        assert_eq!(unknown.window, None);
    }

    /// **A name neither `/models` nor the catalogue knows is refused, and nothing is
    /// spawned** — with the sentence naming `local` and the verb that lists providers, so
    /// the model can correct itself rather than guess.
    #[test]
    fn an_unknown_provider_is_refused_by_name_with_the_fix() {
        let err = subagent_model("definitely-not-a-provider", None)
            .expect_err("an unknown provider must not resolve");
        let said = err.join(" | ");
        assert!(said.contains("definitely-not-a-provider"), "{said}");
        assert!(said.contains("/models"), "{said}");
    }

    /// **The other direction: a cloud model on a child, resolved through the door `/models`
    /// uses.** Guarded on the box actually holding the key — a test that invented one would
    /// be asserting the catalogue rather than the resolver, and skipping is said out loud
    /// rather than passed off as green.
    #[test]
    fn a_cloud_model_resolves_when_the_box_holds_its_key() {
        let has = letibot_provider::Preset::parse("deepseek")
            .ok()
            .is_some_and(|p| letibot_provider::keys::resolve(p, None, None).is_ok());
        if !has {
            eprintln!(
                "SKIPPED: this box holds no deepseek key, so the resolver's cloud arm cannot \
                 be exercised here and THIS IS NOT A PASS."
            );
            return;
        }
        let m = subagent_model("deepseek/deepseek-flash", None).expect("the key is held");
        assert_eq!(m.label, "deepseek/deepseek-flash");
        assert!(m.provider.is_some(), "a cloud child carries its provider");
        assert_eq!(
            m.provider.as_ref().and_then(|p| p.model.clone()).as_deref(),
            Some("deepseek-flash")
        );
        // **A bare provider takes the preset's default model**, the same rule `/models
        // PROVIDER` follows — one resolver, so the child and the session cannot disagree
        // about what a bare name means.
        let bare = subagent_model("deepseek", None).expect("the key is held");
        assert!(bare.label.starts_with("deepseek/"), "{}", bare.label);
    }

    /// **A child nobody can give a window to is refused, by name, rather than run unbounded.**
    ///
    /// The failure this exists to stop is the one three children hit: their wall was computed
    /// from the parent's cloud window (~1M) while the server answering them held 256k, so
    /// nothing compacted and the server ended them. `None` and `Some(None)` are the same answer
    /// for a child, and both are refused; a child with the server's own window is spawned.
    #[test]
    fn a_child_with_no_window_at_all_is_refused_by_name() {
        for unknown in [Some(None), None] {
            let why = child_window_refusal("local", unknown).expect("no window is a refusal");
            assert!(why.contains("`local`"), "names the model: {why}");
            assert!(why.contains("no window"), "says what is wrong: {why}");
            assert!(why.contains("/models"), "and the knob: {why}");
        }
        assert!(
            child_window_refusal("local", Some(Some(262_144))).is_none(),
            "the server's own window is a child with a wall, so it is spawned"
        );
        assert!(child_window_refusal("deepseek/deepseek-flash", Some(Some(1_000_000))).is_none());
    }

    /// **The parent's window is the child's only on the parent's own model** — so the refusal
    /// above stays the width of the defect it was written for, and no wider.
    ///
    /// A `local` child of a `local` session is the same server (and the daemon measures it, so
    /// this branch is the fallback rather than the path). A child named for the parent's own
    /// provider AND model is the parent's number, which is where an explicit
    /// `--context-window` for a model the catalogue does not carry lives. **The case that must
    /// never be true is the last one**: a child on a different model than its parent.
    #[test]
    fn the_parents_window_is_the_childs_only_on_the_parents_own_model() {
        use crate::config::ProviderConfig;
        assert!(
            runs_on_the_parents_model(&None, &None),
            "one server, both local"
        );
        let ds = |model: &str| {
            Some(ProviderConfig {
                name: "deepseek".into(),
                model: Some(model.into()),
                ..Default::default()
            })
        };
        assert!(
            runs_on_the_parents_model(&ds("deepseek-flash"), &ds("deepseek-flash")),
            "same provider and model: the parent's stated number is the child's"
        );
        assert!(
            !runs_on_the_parents_model(&ds("deepseek-chat"), &ds("deepseek-flash")),
            "a different model's window is not the parent's to give"
        );
        assert!(
            !runs_on_the_parents_model(&None, &ds("deepseek-flash")),
            "THE DEFECT: a local child of a cloud session never inherits the cloud window"
        );
        assert!(!runs_on_the_parents_model(&ds("deepseek-flash"), &None));
    }
}

// **The end-to-end test of the merge queue's enqueue, in its own file** — declared here
// because it is a test of THIS module's private `HarnessTaskRunner`, and because a crate
// whose runner is private can only be tested from inside the crate. Its own header says what
// it closes and what it still cannot reach.
#[cfg(test)]
mod queue_e2e;

#[cfg(test)]
mod tests {
    /// **A job this daemon never ran is answered from the record, not refused.**
    ///
    /// The operator, after a restart: entering a job showed nothing at all — `no job ... here`
    /// about a job whose ending was written down. Three claims, and the third is the one a
    /// reader acts on: the state word is the row's own, the bytes are admitted LOST rather than
    /// silently absent, and a redirect names the file that still holds them (R41).
    #[test]
    fn a_recorded_job_says_what_happened_and_where_its_bytes_went() {
        let row = |state: &str, redirect: Option<&str>| JobRecord {
            handle: "j7".into(),
            command: "cargo test --release".into(),
            how: String::new(),
            state: state.into(),
            produced: 4_096,
            elapsed_ms: 8_000,
            redirect: redirect.map(str::to_string),
        };
        let said = recorded_job_lines(&row("exited 0", None)).join("\n");
        assert!(said.contains("exited 0"), "{said}");
        assert!(said.contains("8.0s"), "the duration is the row's: {said}");
        assert!(said.contains("4096 bytes"), "{said}");
        assert!(
            said.contains("not kept"),
            "the bytes are lost and the window must say so rather than look empty: {said}"
        );
        // **A word this function does not know is REPEATED, not interpreted.** A job whose
        // state is `not run (could not join its scope)` must not read here as one that ran,
        // which is why the sentence says *not in this daemon's table* and not *it ran*.
        let never = recorded_job_lines(&row("not run (could not join its scope)", None)).join("\n");
        assert!(
            never.contains("not run (could not join its scope)"),
            "{never}"
        );
        // **And the file, when the row knows it** — R41's other half: the pane cannot read the
        // bytes, so the one useful thing it can do is say where they are.
        let redirected = recorded_job_lines(&row("exited 0", Some("/tmp/build.log"))).join("\n");
        assert!(
            redirected.contains("/tmp/build.log"),
            "a redirected job's file is not named: {redirected}"
        );
    }

    /// **A condition is met by a job that is GONE — including one this daemon never knew** — the
    /// operator's restart case, as the words the model is handed.
    ///
    /// Three fates, three sentences, and the split is the operator's own: a handle that ended here
    /// can be read with `job_output`, and one this daemon has never heard of cannot — so that line
    /// says the result is not here rather than leaving the reader to find out by asking. His
    /// words, which are the whole reason the fate is carried and not just the boolean:
    /// *"you either follow the job result up manually (which is more robust) if next steps depend
    /// on it or just do your things if it was just a timeline"*.
    #[test]
    fn a_conditions_sentence_says_which_kind_of_gone_it_was() {
        assert_eq!(
            fate_line(
                "j121",
                &JobFate::Ended {
                    state: "exited 0".into()
                }
            ),
            "`j121` exited 0",
            "the job's own word, and nothing invented around it"
        );
        let unknown = fate_line("j121", &JobFate::Unknown);
        assert!(
            unknown.contains("not a job this session knows"),
            "the restart case must say the result is not here: {unknown}"
        );
        assert!(
            unknown.contains("read it up yourself"),
            "and say what the reader can do about it, since following it up is the robust half: \
             {unknown}"
        );
        // **A running job is not due, and the sentence exists anyway.** A function that panics on
        // the arm nobody reaches is a function whose contract is a rumour — and this one is
        // reachable the moment anything but `due_todo_lines` asks.
        assert!(
            fate_line("j7", &JobFate::Running).contains("still running"),
            "the running arm must answer rather than be unreachable"
        );
    }

    /// **THE EVALUATOR: what the world says about a handle, and which rows that makes due.**
    ///
    /// The decision a conditioned row turns on, and it had **no assertion at all** before this:
    /// reaching it through a `Harness` needs a backend, a registry, a gate and a store, so the only
    /// thing pinned was [`fate_line`]'s wording — the sentence, and not the choice that picks it.
    ///
    /// The operator's acceptance case is the one that decides the design: *"a todo conditioned on
    /// job end, and then i restart head and harnessd. once server is back it should fire - job is
    /// gone"*. After that restart the process table is empty and the handle is a job this daemon
    /// never ran, so **`Unknown` must meet the condition** — a decision that read *no row, no fate,
    /// skip it* would leave the row waiting for ever on work that is already done, which is the one
    /// failure this whole mechanism exists to prevent.
    #[test]
    fn the_evaluator_fires_a_row_whose_handle_is_gone() {
        use letibot_tokencore::store::{TodoBy, TodoCondition, TodoItem, TodoStatus};
        /// A stored row for one handle, as a restart would find it — the ending written down
        /// before the daemon died.
        fn record(handle: &str, state: &str) -> JobRecord {
            JobRecord {
                handle: handle.into(),
                command: "cargo test".into(),
                how: "asked".into(),
                state: state.into(),
                produced: 12,
                elapsed_ms: 4_000,
                redirect: None,
            }
        }
        let row = |content: &str, when: Option<TodoCondition>| TodoItem {
            content: content.into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when,
        };
        let waiting_on = |handle: &str| {
            Some(TodoCondition::Job {
                handle: handle.into(),
            })
        };

        // **The order of the two looks, which is the whole of the honesty.** The live table answers
        // first: only the process host can say a job runs *now*, so a record saying `running` — a
        // job the daemon was watching when it died — must not override a host that says otherwise.
        assert_eq!(
            fate_of(
                Some(letibot_tools::exec::JobState::Running),
                Some(&record("j7", "running")),
            ),
            JobFate::Running,
            "the live table is the first look, so the record cannot say otherwise"
        );
        assert_eq!(
            fate_of(
                Some(letibot_tools::exec::JobState::Exited { code: 0 }),
                Some(&record("j7", "running")),
            ),
            JobFate::Ended {
                state: "exited 0".into()
            },
            "the host's own word for it, and not the stale row's"
        );
        // **The restart, which is why the record is the second look**: no process table at all, and
        // the ending this daemon wrote down before it went away.
        assert_eq!(
            fate_of(None, Some(&record("j121", "exited 3"))),
            JobFate::Ended {
                state: "exited 3".into()
            },
            "the record is what survives a restart"
        );
        // And a handle nobody has run: met, and not dressed up as an ending — the distinction is
        // the operator's, since the result can be read up for one and not for the other.
        assert_eq!(
            fate_of(None, None),
            JobFate::Unknown,
            "a job this daemon never heard of has ended; that is what a restart looks like"
        );

        // **The filter, over a board the world answers for.** Two skips — a row with no condition,
        // and a running job — and two firings, one of each kind of gone.
        let rows = vec![
            row("ship the parity row", None),
            row("push once CI lands", waiting_on("j121")),
            row("wait for the build", waiting_on("j7")),
            row("after the restart", waiting_on("j900")),
        ];
        let due = due_rows(&rows, |handle| match handle {
            "j7" => JobFate::Running,
            "j121" => JobFate::Ended {
                state: "exited 0".into(),
            },
            _ => JobFate::Unknown,
        });
        assert_eq!(
            due,
            vec![
                (
                    "push once CI lands".to_string(),
                    "j121".to_string(),
                    JobFate::Ended {
                        state: "exited 0".into()
                    }
                ),
                (
                    "after the restart".to_string(),
                    "j900".to_string(),
                    JobFate::Unknown
                ),
            ],
            "a row with no condition is not due, a running job is not due, and both kinds of gone \
             are — with the handle the caller needs to consume the condition: {due:?}"
        );
    }

    /// **A subagent's ask is addressed to the tree's ROOT, never to its parent** — R58's ask
    /// route, and the one sentence in the spawn that has to be right for a card to reach a
    /// head at all.
    ///
    /// The GRANDCHILD case is what decides it: at depth ≥ 1 the parent is itself a child, so
    /// addressing the parent would post the card on a session no head can be attached to and
    /// no worker drives — a card nobody would ever see, which is the defect this whole change
    /// is against. The fixture is therefore a root, a child and a grandchild, built the way
    /// `jobwatch`'s settlement test builds them.
    #[test]
    fn a_subagents_ask_is_addressed_to_the_tree_root_and_not_to_its_parent() {
        use letibot_sessionlog::registry::{Registry, SessionWiring};

        let registry = Registry::new();
        // The root is the session a daemon drives; the child and the grandchild are the ones
        // it does not. All three are registered, which is what `adopt` does at a spawn.
        let root_hub = registry
            .create("s-root", "", SessionWiring::default())
            .expect("the root registers");
        let _child_hub = registry
            .create_under(
                "s-child",
                "",
                SessionWiring::default(),
                Some("s-root".into()),
            )
            .expect("the child registers");
        let grandchild_hub = registry
            .create_under(
                "s-child-sub-1",
                "",
                SessionWiring::default(),
                Some("s-child".into()),
            )
            .expect("the grandchild registers");

        // The answers the whole tree reaches: the root's own, handed down through `Parts`.
        let answers = Some(Arc::new(crate::answers::Answers::new()));
        // The grandchild's set is built FROM the root's, which is what gives it the ROOT —
        // the same set `Parts` carries for a grandchild.
        let root_watch = crate::jobwatch::JobWatchers::watching_tasks(&root_hub, None);
        let grandchild_watch = crate::jobwatch::JobWatchers::watching_tasks(&grandchild_hub, None)
            .shares_tree(&root_watch);

        let (root, head) = subagent_ask_target(
            &registry,
            Some(&grandchild_watch),
            "s-child",
            answers.as_ref(),
        );
        assert_eq!(
            root, "s-root",
            "the card was addressed to the parent, not to the tree's root"
        );
        assert!(
            head.is_some(),
            "the root's answers did not reach the child's ask"
        );

        // **A tree with no head-wired adjudicator**: no card is posted, and the child's own
        // refusal names the root instead (see `answers::SubagentAdjudicator`).
        let (root, head) = subagent_ask_target(&registry, Some(&grandchild_watch), "s-child", None);
        assert_eq!(root, "s-root");
        assert!(head.is_none());

        // **A root this daemon does not hold** — a harness with no daemon behind it, or a
        // registry that has lost the session — is the same refusal rather than a card on a
        // hub nothing will read. With no watcher set at all, the parent IS the tree's root.
        let (root, head) = subagent_ask_target(&registry, None, "s-absent", answers.as_ref());
        assert_eq!(root, "s-absent");
        assert!(head.is_none(), "a hub nobody holds cannot answer");
    }

    /// **A row announced through a capturing sink is filled by the pairing rule, and
    /// body-less without it** — the compaction defect at the smallest size there is.
    ///
    /// This is the free half of `Harness::reconcile` (`reconcile_rows`), driven the way
    /// the summary-turn helper drives it. The false-first direction is the assertion that
    /// matters: a sink announces on `emit` and carries the id, so a caller that drops the
    /// sink has announced a row it never filled — which is what the daemon's view then
    /// serves to every head and to every `/resync`.
    #[test]
    fn an_announced_row_is_filled_by_the_pairing_rule_and_body_less_without_it() {
        use super::*;
        let item = || TranscriptItem::System {
            text: "the summary instruction".into(),
            origin: letibot_transcript::SystemOrigin::Update,
        };
        let announce = |sink: &mut CapturingSink| {
            sink.emit(letibot_turn::TurnEvent::TranscriptAppended {
                item_id: "s#t0.9".into(),
                kind: "system",
                ledger_head: "beef".into(),
                tokens: 3,
            });
        };

        // The defect: announced, sink dropped, no body. The view holds the row.
        let hub = Hub::new("s");
        let mut sink = CapturingSink::new(hub.clone());
        announce(&mut sink);
        drop(sink);
        let snap = hub.snapshot();
        assert_eq!(snap.items.len(), 1, "the announcement reached the log");
        assert!(
            snap.items[0].item.is_none(),
            "the premise: nothing filled it"
        );

        // The rule: same announcement, paired. The row carries its body.
        let hub = Hub::new("s");
        let mut sink = CapturingSink::new(hub.clone());
        announce(&mut sink);
        reconcile_rows(&hub, &mut sink, std::slice::from_ref(&item()));
        let snap = hub.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert!(
            snap.items[0].item.is_some(),
            "announced and never filled: {:?}",
            snap.items[0].item_id
        );
    }
    /// **The scratch is per-user and per-process, never the flat world-readable
    /// `/tmp/letibot-scratch-<pid>`** — the operator's ask, 2026-10-05: *"job output
    /// should go to your scratch directory."* The path is checked by SHAPE rather than
    /// by resolving `$XDG_RUNTIME_DIR`, which a test must not mutate process-wide: the
    /// old spelling is refused outright, and the directory component the process's
    /// scratch sits in is the per-user one either environment names.
    #[test]
    fn the_scratch_dir_is_per_user_and_never_the_flat_tmp_spelling() {
        let s = super::scratch_dir();
        let said = s.display().to_string();
        assert!(
            !said.contains("letibot-scratch"),
            "the old world-readable spelling: {said}"
        );
        assert!(
            s.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("scratch-")),
            "the last component is the process's own: {said}"
        );
        // Under the runtime dir when there is one — both components of it.
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            assert!(
                s.starts_with(std::path::Path::new(&rt).join("letibot")),
                "the runtime dir's letibot, where the socket already lives: {said}"
            );
        }
    }

    /// **The depth cap refuses by NAME, and at the right boundary** (R58).
    ///
    /// `task` is seated on `m2_coder` at every depth, so the seat table is not where the
    /// limit lives — this function is. `depth == max` is the `task` call that would open
    /// `max + 1` and is refused; `depth == max - 1` is the last one allowed.
    #[test]
    fn the_depth_cap_refuses_the_spawn_one_past_it_by_name() {
        // A root (0) with the ruled default (3) may spawn, and so may levels 1 and 2.
        assert!(super::subagent_depth_refusal(0, 3).is_none());
        assert!(super::subagent_depth_refusal(1, 3).is_none());
        assert!(super::subagent_depth_refusal(2, 3).is_none());
        // Depth 3 is the last level; `task` there would open depth 4 and is refused.
        let why = super::subagent_depth_refusal(3, 3).expect("refused at the cap");
        assert!(
            why.contains("depth 3"),
            "the refusal names where it is: {why}"
        );
        assert!(
            why.contains("depth 4"),
            "and the level it would have opened: {why}"
        );
        assert!(
            why.contains("--max-subagent-depth"),
            "and the knob that moves it — a refusal that does not name the knob is how a \
             model manufactures the workaround this seat already paid for once: {why}"
        );
        assert!(
            why.contains("Nothing was started"),
            "a refusal is not a maybe: {why}"
        );
        // A cap of 0 refuses even a root — *no nesting*, read honestly rather than as
        // *unlimited*.
        assert!(super::subagent_depth_refusal(0, 0).is_some());
    }

    /// **A real worktree is arranged the way `task_start` arranges it**: the branch is
    /// `agent/<slug>`, cut from the base, and the main tree is untouched.
    ///
    /// This drives the same `git_in` door `HarnessTaskRunner::start_worktree` uses, in a
    /// throwaway repo, so the branch name and the main tree's cleanliness are facts on
    /// disk rather than a claim in the answer. The operator's ask — *"we will need a new
    /// tool - task_start or what that will arrange worktree, firecode and subagent"* — is
    /// that the placement is arranged before the child is spawned, and this is the half of
    /// that that can be checked without a live subagent.
    #[test]
    fn a_worktree_is_arranged_with_its_branch_and_the_main_tree_untouched() {
        use std::process::Command;
        // A throwaway repo, the same shape `detect.rs`'s tests use.
        let root = std::env::temp_dir().join(format!(
            "letibot-task-start-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
        git(&["add", "-A"]);
        git(&["commit", "-qm", "first"]);
        let base_sha =
            super::git_in(root.to_str().unwrap(), &["rev-parse", "HEAD"]).expect("the base SHA");

        // **The worktree, arranged the way `start_worktree` arranges it.**
        let slug = "task-start";
        let path = letibot_tools::builtins::task::worktree_path(root.to_str().unwrap(), slug);
        let branch = letibot_tools::builtins::task::worktree_branch(slug);
        super::git_in(
            root.to_str().unwrap(),
            &["worktree", "add", &path, "-b", &branch, "HEAD"],
        )
        .expect("the worktree is created");

        // **The branch is `agent/<slug>`, cut from the base.**
        let wt_branch = super::git_in(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .expect("the worktree's branch");
        assert_eq!(wt_branch, branch, "the branch is agent/<slug>");
        let wt_base = super::git_in(&path, &["rev-parse", "HEAD"]).expect("the worktree's HEAD");
        assert_eq!(wt_base, base_sha, "the branch is cut from the base");

        // **The main tree is untouched**: its branch is still `main`, its HEAD is still
        // the base, and its status is clean — the worktree did not move the main tree.
        let main_branch = super::git_in(
            root.to_str().unwrap(),
            &["rev-parse", "--abbrev-ref", "HEAD"],
        )
        .expect("the main tree's branch");
        assert_eq!(main_branch, "main", "the main tree's branch is untouched");
        let main_head = super::git_in(root.to_str().unwrap(), &["rev-parse", "HEAD"])
            .expect("the main tree's HEAD");
        assert_eq!(main_head, base_sha, "the main tree's HEAD is untouched");
        // The `.claude/` directory is where the worktree lives, and it is untracked in
        // the main tree — that is expected, it is the worktree's home. The main tree's
        // *tracked* files are what must be untouched, so the status excludes it.
        let status = super::git_in(
            root.to_str().unwrap(),
            &["status", "--porcelain", "--", ".", ":(exclude).claude"],
        )
        .expect("the main tree's status");
        assert!(
            status.is_empty(),
            "the main tree's tracked files are untouched: {status}"
        );

        // **And the worktree is a fact on disk**: `git worktree list` shows it.
        let list = super::git_in(root.to_str().unwrap(), &["worktree", "list"])
            .expect("the worktree list");
        assert!(list.contains(&path), "the worktree is listed: {list}");

        // Clean up: remove the worktree and the repo.
        let _ = super::git_in(
            root.to_str().unwrap(),
            &["worktree", "remove", "--force", &path],
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The subagent's title is not the session's name, and the two have different bounds.**
    ///
    /// A session name is an **address** — `@seat/title`, which somebody types — so
    /// `derive_title` holds it to six words and 48 characters. A subagent's title is a **row**,
    /// and the operator's ruling is that a row's cap is the UI's: *"subagents list has
    /// everything capped at 48 chars. but this cap is a ui concern — for example my terminal is
    /// 160 cells wide, make the cap 256."* Reusing the address rule for the row cut every
    /// subagent line to a name's length. This pins both, so a later reader cannot quietly merge
    /// them again.
    #[test]
    fn a_subagent_title_is_a_row_and_a_session_name_is_an_address() {
        let long = "audit the session store and say which rows are never read by anything at \
                    all in the whole tree, and then propose a fix for each";
        // The ADDRESS rule: six words and no more, 48 characters at the outside.
        assert_eq!(
            super::derive_title(long),
            "audit the session store and say",
            "a name is six words, so it stays a name somebody can type"
        );
        // The DISPLAY rule: the whole first line, and the six-word rule does not apply.
        let title = super::subagent_title(long);
        assert_eq!(
            title, long,
            "a title long enough to matter must survive whole"
        );
        assert!(
            title.split_whitespace().count() > 6,
            "the naming rule must not reach a title: {title}"
        );
        // Only the first line — a row is one line — and only past the generous bound.
        assert_eq!(
            super::subagent_title("audit the store\nand the reader"),
            "audit the store"
        );
        let huge = "x".repeat(400);
        let cut = super::subagent_title(&huge);
        assert_eq!(cut.chars().count(), super::SUBAGENT_TITLE_MAX);
        assert!(cut.ends_with('…'), "{cut}");
    }

    /// **The child's row says where its point came from, and says when it was clamped.**
    ///
    /// The row's vocabulary is the one the settings table already uses for a mode's source, so
    /// the phrase `inherited from the parent session` is asserted here rather than trusted, and
    /// the clamp's sentence is asserted to survive INTO the row — a child silently at a
    /// different point than its parent is the state that made this a bug hunt.
    ///
    /// **What this does NOT test**, and it is worth naming rather than leaving to be assumed:
    /// that the spawn reads the parent's live point. That is `HarnessTaskRunner::point` reading
    /// a cell `Harness::set_mode_consented` writes, and reaching either needs a session with a
    /// model behind it — no test in this crate opens a `Harness` (they all need a vocabulary
    /// GGUF and a server), so the wiring is read rather than driven. The DECISION is tested
    /// where it lives: `mode::tests::a_child_inherits_the_point_of_the_session_that_spawned_it`
    /// and its neighbours.
    #[test]
    fn a_childs_row_says_where_its_point_came_from() {
        let inherited = super::child_mode_source(None);
        assert_eq!(inherited, "inherited from the parent session");

        let clamped = letibot_tools::mode::Mode::ALLOW_ALL.inherited_by(
            &[letibot_tools::mode::Prereq::ReachableAdjudicator],
            letibot_tools::mode::Seats {
                write: false,
                exec: true,
                network: false,
            },
            "runner",
        );
        let why = clamped.why().expect("a confinement is missing here");
        let said = super::child_mode_source(Some(why));
        assert!(said.starts_with(&inherited), "{said}");
        assert!(said.contains("clamped"), "{said}");
        assert!(said.contains("`runner`"), "the seat is named: {said}");
        assert!(said.contains("confinement"), "what was missing: {said}");
        assert!(said.contains("`writes allowed`"), "where it landed: {said}");
    }

    /// **Three children minted at one instant get three ids.**
    ///
    /// leticl found this on the wire and could not read this tree: three `task` calls in a
    /// loop, one id — *"one pane row, no subagent counter, one `[task]` notice — and two of
    /// the three answers never reach me at all"*. The id is the handle `task_result`
    /// collects by, and it finds the FIRST slot under a handle, so the other replies exist,
    /// settle, and are unreachable.
    ///
    /// The DEFECT was a millisecond clock, which three calls in one round share trivially;
    /// this drives the same instant down to a nanosecond, because the guard has to hold at
    /// the resolution the mint now uses and not merely at the one it was found at.
    ///
    /// **Both halves of the guard are exercised, because either alone misses the real
    /// case.** The handed-out check is the one that catches the ordinary shape: `adopt`
    /// happens inside the child's thread, after a whole harness has been opened, while the
    /// mint returns at once — so three calls in one round all mint before any of them
    /// adopts, and a check against the registry alone finds nothing.
    #[test]
    fn three_children_minted_at_one_instant_get_three_ids() {
        use letibot_sessionlog::registry::{Registry, SessionWiring};

        const NS: u128 = 1_700_000_000_000_000_000;
        let r = Registry::new();
        let mut handed: Vec<String> = Vec::new();
        let mint = |handed: &Vec<String>| {
            super::mint_sub_id_at(&r, "s-parent", NS, |id| handed.iter().any(|h| h == id))
        };

        let a = mint(&handed);
        handed.push(a.clone());
        let b = mint(&handed);
        handed.push(b.clone());
        let c = mint(&handed);
        assert_eq!(
            a,
            format!("s-parent-sub-{NS}"),
            "the first keeps the plain form"
        );
        assert_eq!(b, format!("s-parent-sub-{NS}-2"));
        assert_eq!(c, format!("s-parent-sub-{NS}-3"));
        assert_ne!(
            a, b,
            "two children, one id, is two answers nobody can collect"
        );
        assert_ne!(b, c);
        assert_ne!(a, c);

        // And the registry half: an id whose child is already ADOPTED is refused too, even
        // by a caller that believes nothing has been handed out.
        let adopted = super::mint_sub_id_at(&r, "s-parent", NS, |_| false);
        r.adopt(
            r.new_hub(adopted.clone()),
            "",
            SessionWiring::default(),
            None,
        )
        .expect("the first child adopts");
        let next = super::mint_sub_id_at(&r, "s-parent", NS, |_| false);
        assert_ne!(next, adopted);
    }

    /// **A stored pair measures ONE conversation, and it is used only for that
    /// one.** See [`super::recovered_scale`].
    ///
    /// The defect this closes, measured 2026-10-02: a store holding 940,211 —
    /// the provider's count for a ~950k-token conversation — was paired with a
    /// ledger of 1,486,369 that the daemon had just rebuilt, because nothing
    /// recorded which conversation the number belonged to. The ratio came out
    /// 0.63 where the truth was 1.016, and the window 1,580,888 instead of about
    /// 1,015,628 — so a session 1.46M tokens deep planned against a window it was
    /// already far past, never compacted, and died at the provider's 400 on every
    /// attempt.
    #[test]
    fn a_stored_token_pair_is_used_only_for_the_conversation_it_measured() {
        // The pair that matches: both halves of one measurement, and the ledger
        // this daemon has rebuilt is the same size.
        assert_eq!(
            super::recovered_scale(Some(940_211), Some(940_211), 940_211),
            Some((940_211, 940_211)),
            "a pair measured on this conversation IS the ratio"
        );
        // **The bug.** The same count, which was true — of a smaller conversation.
        // Smaller is the dangerous direction: divided into a bigger ledger it
        // OVERSTATES the ratio, so the window grows and compaction comes too late.
        assert_eq!(
            super::recovered_scale(Some(940_211), Some(940_211), 1_486_369),
            None,
            "a count from a smaller conversation must not be paired with this ledger"
        );
        // A row written before v12 has no ledger to check against. Unverifiable is
        // not the same as wrong, but it IS the same as unmeasured — and the
        // unscaled window is the honest answer for unmeasured.
        assert_eq!(super::recovered_scale(Some(940_211), None, 940_211), None);
        // No provider count at all: a row whose turn never finished.
        assert_eq!(super::recovered_scale(None, Some(940_211), 940_211), None);
        // Zero is not a measurement of anything, on either side.
        assert_eq!(
            super::recovered_scale(Some(0), Some(940_211), 940_211),
            None
        );
        assert_eq!(super::recovered_scale(Some(940_211), Some(0), 0), None);
    }

    /// **Infinity is `0`, and it is one line, so it gets one test.**
    ///
    /// The operator: *"make 200 tool calls limit configurable and set it to
    /// infinity"*. `0` reaching the loop as `0` would mean a turn that may take no
    /// rounds at all — the exact opposite — and it is the kind of inversion that
    /// looks right in a diff.
    #[test]
    fn a_zero_backstop_is_unbounded_and_every_other_number_is_itself() {
        assert_eq!(super::round_backstop(0), usize::MAX, "0 means no bound");
        assert_eq!(super::round_backstop(1), 1);
        assert_eq!(super::round_backstop(200), 200);
        // And the default is the unbounded one, which is the half of this the
        // operator actually asked for.
        assert_eq!(
            super::round_backstop(Config::for_this_box("/tmp").max_tool_rounds),
            usize::MAX
        );
    }

    /// **`next` is the offset to ask at, not a count, and it stops at the end.**
    ///
    /// A pane pages by handing `next` straight back as the next request's offset,
    /// so it has to be the offset the window ended at — and it has to be `None`
    /// exactly when the ring holds nothing further, or the pane offers a page that
    /// comes back empty and reads as the job's end.
    #[test]
    fn a_job_window_names_the_next_offset_until_the_ring_runs_out() {
        let slice = |from, to, produced, dropped, retained| letibot_tools::exec::OutputSlice {
            bytes: Vec::new(),
            from,
            to,
            produced,
            dropped,
            retained,
        };
        // A window in the middle of a long job: there is more, and `next` is where
        // this one ended.
        assert_eq!(next_job_offset(&slice(0, 16, 40, 0, 40)), Some(16));
        // The window has taken everything still held: no further page.
        assert_eq!(next_job_offset(&slice(0, 40, 40, 0, 40)), None);
        // A ring that dropped its front: the end of what is *held* is the end, even
        // though `produced` is past `to` — that part is gone, not waiting.
        assert_eq!(next_job_offset(&slice(992, 1000, 1000, 992, 8)), None);
        // And a window that stopped short of the retained tail still has a page.
        assert_eq!(next_job_offset(&slice(992, 996, 1000, 992, 8)), Some(996));
    }

    /// **THE WINDOW OF A REDIRECTED JOB IS THE FILE'S TAIL** — R41's other half, and the
    /// operator's own next ask after the pane learned to name the file: *"job pane can tail
    /// the file"*.
    ///
    /// `offset == 0` is the TAIL, because that is what a fresh read of an output pane means;
    /// any other offset is a forward window from there, so `→` still walks to the end of a
    /// long log the way it does for the ring.
    #[test]
    fn a_redirected_jobs_window_is_the_tail_of_its_file() {
        let dir = std::env::temp_dir().join(format!("letibot-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("build.log");
        let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &body).expect("write");
        let path = path.to_str().expect("utf-8 path");

        // **A fresh read is the tail**: the last `limit` bytes, and the pane is told the
        // FILE's length so its `of N` is the file and not the window.
        let w = tail_file(path, 0, 20).expect("a window");
        assert_eq!(w.produced, body.len() as u64);
        assert_eq!(
            w.to,
            body.len() as u64,
            "the tail ends at the end of the file"
        );
        assert_eq!(w.from, body.len() as u64 - 20);
        assert_eq!(w.text.len(), 20);
        assert!(w.text.ends_with("line 39\n"), "{:?}", w.text);

        // **An explicit offset is a forward window**, which is what `next` hands to `→`.
        let w2 = tail_file(path, w.from, 20).expect("a window");
        assert_eq!(w2.from, w.from);
        assert_eq!(w2.to, w.from + 20);

        // A limit larger than the file is the whole file, not an error and not padding.
        let all = tail_file(path, 0, 4096).expect("a window");
        assert_eq!(all.from, 0);
        assert_eq!(all.to, body.len() as u64);

        // **A file that is not there is `None`** — the job has not created it yet — which is
        // what the caller answers with the sentence that names the path.
        assert!(tail_file(&dir.join("nope.log").to_string_lossy(), 0, 10).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;
    use letibot_tools::authorise::TrailProvenance;

    /// **A killed subagent settles as STOPPED, not as having answered.**
    ///
    /// An interrupted turn still returns the text it had written when the interrupt
    /// landed, so `run_to_completion` comes back `Ok` — and a `task_result` that called
    /// that the child's last word would hand the parent a truncated answer with nothing on
    /// it to say it had been cut off. The same defect as a truncated summary presented as
    /// whole, one layer down.
    #[test]
    fn a_killed_subagent_settles_as_stopped_rather_than_as_an_answer() {
        use letibot_tools::builtins::task::TaskStatus;

        // Unkilled: the answer is the answer. This arm is here because the kill flag must
        // not become the relabelling of every completion that happens to follow one.
        let plain = TaskSlot::new("s-parent");
        plain.settle(TaskStatus::Done {
            answer: "half a sentence".into(),
        });
        assert!(matches!(
            &*plain.state.lock().expect("slot"),
            TaskStatus::Done { .. }
        ));

        let killed = TaskSlot::new("s-parent");
        killed.kill();
        killed.settle(TaskStatus::Done {
            answer: "half a sentence".into(),
        });
        match &*killed.state.lock().expect("slot") {
            TaskStatus::Failed { why } => assert!(
                why.contains("job_kill"),
                "the reason names the stop, so a reader can tell it from a crash: {why}"
            ),
            other => panic!("a killed subagent settled as {other:?}"),
        }

        // **A real failure is not relabelled.** The flag must not overwrite the child's
        // own reason for dying, which is the more useful sentence of the two.
        let failed = TaskSlot::new("s-parent");
        failed.kill();
        failed.settle(TaskStatus::Failed {
            why: "the model went away".into(),
        });
        assert!(matches!(
            &*failed.state.lock().expect("slot"),
            TaskStatus::Failed { why } if why.contains("went away")
        ));
    }

    /// **A stop takes THIS session's children and nobody else's, and not the ones that are
    /// gone** — the downward edge of the tree, at the size it can be asserted at.
    ///
    /// The list is the TREE's (`Parts::tree_slots`: one list, so any ancestor can `task_result`
    /// any handle in it), which is exactly why stopping needs the owner back: a root that
    /// stopped every slot it could see would stop its own children AND their children in one
    /// go, and a child would stop its siblings' work. The operator's design says otherwise —
    /// *"we talk to parents and they own lifecycle"* — and one level per session is what makes
    /// the walk down finite and each session answerable for exactly its own.
    ///
    /// The second filter is the one that was measured into existence: a child that has ANSWERED
    /// is still alive (a host child parks in `serve_child` and can be asked more) while a child
    /// whose thread has ended has no reader for an interrupt at all. So "has an answer" cannot
    /// stand for "is gone", and `TaskSlot::exited` is the fact that can.
    #[test]
    fn a_stop_takes_this_sessions_children_and_not_the_ones_that_are_gone() {
        // The tree's one list, with four entries: three of mine — one still computing, one
        // parked with an answer, one whose thread has ended — and one of my PARENT's, which is
        // a sibling of mine and must not be in my answer.
        let mine_running = Arc::new(TaskSlot::new("s-child"));
        let mine_answered = Arc::new(TaskSlot::new("s-child"));
        mine_answered.settle(letibot_tools::builtins::task::TaskStatus::Done {
            answer: "done, and still here".into(),
        });
        let mine_and_gone = Arc::new(TaskSlot::new("s-child"));
        mine_and_gone.mark_exited();
        // **A slot's handle follows its OWNER** (`<owner>-sub-N`, the shape
        // `HarnessTaskRunner::start` mints), so the parent's child cannot be called
        // `s-child-sub-*`: a name that says `s-child` on a slot the root started is the
        // confusion this whole test is about, and the owner field is what settles it.
        let my_parents = Arc::new(TaskSlot::new("s-root"));
        let slots = vec![
            ("s-child-sub-1".to_string(), mine_running),
            ("s-child-sub-2".to_string(), mine_answered),
            ("s-child-sub-3".to_string(), mine_and_gone),
            ("s-root-sub-1".to_string(), my_parents),
        ];

        let stop = stoppable_children(&slots, "s-child");
        assert_eq!(
            stop,
            vec!["s-child-sub-1".to_string(), "s-child-sub-2".to_string()],
            "my own children — the running one AND the one that has answered and is still \
             parked — and no sibling's, no parent's, and none that has left"
        );
        // And from the other seat in the same tree: the root stops ITS children, not this
        // session's. Same list, different answer — which is the whole reason the owner is on
        // the slot.
        assert_eq!(
            stoppable_children(&slots, "s-root"),
            vec!["s-root-sub-1".to_string()],
            "a parent's stop is its own children, not its grandchildren's"
        );
        assert!(
            stoppable_children(&slots, "s-nobody").is_empty(),
            "a session with no children of its own in this tree stops nothing"
        );
    }

    /// **The sentence a stopped session leaves names what it stopped, and what it could not.**
    ///
    /// The pure half of `stop_children_first`, and the reason it is asserted rather than the
    /// `Warning` around it: a count that swallowed a refusal would be the claim-versus-fact
    /// defect this tree keeps paying for — a reader told *stopped 2* while one of them is still
    /// computing, and nothing anywhere saying so.
    #[test]
    fn a_stop_says_what_it_stopped_and_what_it_could_not() {
        // No children: no sentence at all, which is the ordinary case — a session with nothing
        // running says nothing on the way out.
        assert_eq!(children_stopped_notice(&[]), None);

        let stopped = children_stopped_notice(&[(
            "s-child-sub-1".into(),
            Ok("the daemon accepted the interrupt".into()),
        )]);
        let said = stopped.expect("one child stopped is one sentence");
        assert!(said.contains("s-child-sub-1"), "{said}");
        assert!(
            said.contains("before stopping itself"),
            "the order is the point of it — children first, then this session: {said}"
        );

        // A refusal is NAMED, with the daemon's own reason, and the sentence does not claim a
        // stop it did not get.
        let mixed = children_stopped_notice(&[
            ("s-a".into(), Ok("accepted".into())),
            (
                "s-b".into(),
                Err("`s-b` has already settled, or its session is gone".into()),
            ),
        ])
        .expect("a partial stop is still a sentence");
        assert!(mixed.contains("s-a"), "{mixed}");
        assert!(mixed.contains("s-b"), "{mixed}");
        assert!(
            mixed.contains("could NOT be stopped") && mixed.contains("already settled"),
            "the refusal is on the record, in the daemon's own words: {mixed}"
        );
    }

    /// **A stop delivered to a running turn takes the session's CHILDREN with it first** — the
    /// operator's design in one line, *"we talk to parents and they own lifecycle"*, and the
    /// door an interrupt actually reaches a turn through.
    ///
    /// The instrument is a fake runner, because the claim is about WHO IS TOLD and WHEN rather
    /// than about anything a child does: the parent is stopped by the very message the arm
    /// returns, and the children have to be told before it — a stop that waited for its subtree
    /// would hold the turn it is stopping open.
    ///
    /// **Two levels, because the recursion is the shape.** Each session's arm stops ITS OWN
    /// runner and no other; the walk down is then one level per session, which is what makes it
    /// finite and what makes every session answerable for exactly its own children.
    #[test]
    fn a_stop_takes_the_sessions_children_before_it_takes_the_session() {
        /// Records that it was asked to stop, and what it was asked for after each stop.
        struct Recording {
            me: String,
            stops: Arc<Mutex<Vec<String>>>,
        }

        impl letibot_tools::builtins::task::TaskRunner for Recording {
            fn start(
                &self,
                _prompt: &str,
                _spec: &letibot_tools::builtins::task::TaskSpec,
            ) -> Result<String, String> {
                Err("this runner starts nothing".into())
            }
            fn collect(
                &self,
                _handle: &str,
                _timeout: std::time::Duration,
            ) -> letibot_tools::builtins::task::TaskStatus {
                letibot_tools::builtins::task::TaskStatus::Unknown
            }
            fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
                self.stops.lock().expect("stops").push(self.me.clone());
                vec![(
                    format!("{}-sub-1", self.me),
                    Ok("the daemon accepted the interrupt".into()),
                )]
            }
        }

        // Two levels of the same door: a parent's steering source and a child's, each with its
        // OWN runner — which is what "one level per session" means in code.
        let stops = Arc::new(Mutex::new(Vec::new()));
        let parent_hub = Hub::new("s-root");
        let mut parent = HubSteering {
            hub: parent_hub.clone(),
            trail: None,
            injected: Arc::new(Mutex::new(VecDeque::new())),
            monitors: None,
            monitor_cursor: Arc::new(AtomicUsize::new(0)),
            job_watch: None,
            subagents: Some(Arc::new(Recording {
                me: "s-root".into(),
                stops: stops.clone(),
            })),
            held: VecDeque::new(),
        };
        let child_hub = Hub::new("s-child");
        let mut child = HubSteering {
            hub: child_hub.clone(),
            trail: None,
            injected: Arc::new(Mutex::new(VecDeque::new())),
            monitors: None,
            monitor_cursor: Arc::new(AtomicUsize::new(0)),
            job_watch: None,
            subagents: Some(Arc::new(Recording {
                me: "s-child".into(),
                stops: stops.clone(),
            })),
            held: VecDeque::new(),
        };

        // Nothing has been stopped to begin with, and a prompt is not a stop: it is a turn.
        assert!(stops.lock().expect("stops").is_empty());
        let head = parent_hub.attach(
            "tui",
            "dead@lab2x1",
            letibot_sessionlog::protocol::Caps::default(),
            0,
        );
        parent_hub.submit(
            &head.head_id,
            "c1",
            0,
            letibot_sessionlog::CommandKind::Prompt {
                text: "carry on".into(),
            },
        );
        assert!(
            matches!(parent.try_next(), Some(m) if !m.urgent),
            "a prompt is ordinary steering"
        );
        assert!(
            stops.lock().expect("stops").is_empty(),
            "and it stops nothing: children are taken by a STOP, not by every message"
        );

        // The stop, on both sessions.
        for (hub, steering) in [(&parent_hub, &mut parent), (&child_hub, &mut child)] {
            let f = hub.submit(
                letibot_sessionlog::hub::DAEMON_SUBMITTER,
                "job_kill-1",
                0,
                letibot_sessionlog::CommandKind::Interrupt {
                    reason: "`job_kill` stopped this subagent".into(),
                },
            );
            assert!(
                matches!(f, letibot_sessionlog::ServerFrame::Accepted { .. }),
                "the hub refused the interrupt: {f:?}"
            );
            let got = steering.try_next();
            assert!(
                matches!(&got, Some(m) if m.urgent),
                "the interrupt is handed to the engine as urgent, which is what stops the \
                 turn: {got:?}"
            );
        }
        assert_eq!(
            *stops.lock().expect("stops"),
            vec!["s-root".to_string(), "s-child".to_string()],
            "each session's own arm stopped its own children, in that order"
        );

        // **The ordering, which is the requirement and not tidiness.** The arm stops the
        // children on the way IN, before the urgent message it returns can stop the session —
        // and the notice is on the stopping session's own log, where the person who pressed
        // Esc is looking.
        let said = |hub: &Arc<Hub>| {
            hub.retained()
                .iter()
                .filter_map(|e| match &e.event {
                    letibot_sessionlog::SessionEvent::Warning { code, detail, .. }
                        if code == "subagents_stopped" =>
                    {
                        Some(detail.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let root_said = said(&parent_hub);
        assert_eq!(root_said.len(), 1, "one sentence per stop: {root_said:?}");
        assert!(
            root_said[0].contains("s-root-sub-1")
                && root_said[0].contains("before stopping itself"),
            "{}",
            root_said[0]
        );
        let child_said = said(&child_hub);
        assert!(
            child_said[0].contains("s-child-sub-1"),
            "and the child's own log names the child's own child — one level, each session \
             answerable for its own: {child_said:?}"
        );
    }

    /// **R7's tool-side half: the sentence says the result comes to you.**
    ///
    /// The text is the deliverable — it is what taught the model that waiting was the
    /// only way to learn a result — so what it says is asserted rather than assumed. Two
    /// things must be in it: the job's own identity (id, command, how it ended), and the
    /// promise that there is nothing to wait for.
    #[test]
    fn a_completion_notice_names_the_job_and_says_do_not_wait() {
        let text = completion_notice(
            &[JobCompletion {
                kind: BackgroundKind::Job,
                owner: "s-me".into(),
                job: "j7".into(),
                command: "cargo build --release".into(),
                state: "exited 0".into(),
                produced: 4096,
                elapsed_ms: 4_400,
                detail: String::new(),
            }],
            true,
        );
        assert!(text.starts_with("[job]"), "labelled like a monitor: {text}");
        assert!(text.contains("`j7`"), "{text}");
        assert!(text.contains("exited 0"), "{text}");
        assert!(text.contains("cargo build --release"), "{text}");
        assert!(text.contains("4.4s"), "how long it ran: {text}");
        assert!(text.contains("4096"), "how much it wrote: {text}");
        // The half that is the fix, not the footnote.
        assert!(text.contains("do not need to wait"), "{text}");
        // And it must not inline the output — only say where it is.
        assert!(text.contains("job_output"), "{text}");
    }

    /// A job whose view was already reaped still produces a usable notice: the command
    /// is the one field the durable settlement does not carry, so it is the one that can
    /// be missing, and the notice says so rather than inventing one.
    #[test]
    fn a_reaped_jobs_completion_says_the_command_was_not_recorded() {
        let text = completion_notice(
            &[JobCompletion {
                kind: BackgroundKind::Job,
                owner: "s-me".into(),
                job: "j9".into(),
                command: String::new(),
                state: "gone".into(),
                produced: 0,
                elapsed_ms: 0,
                detail: String::new(),
            }],
            true,
        );
        assert!(text.contains("command not recorded"), "{text}");
        assert!(!text.contains("``"), "an empty pair of backticks: {text}");
    }
    /// **The child's sentence, which had no test, and it is the one the model reads.**
    ///
    /// `completion_notice`'s twin on the subagent side: same channel, same wake, and the text is
    /// again the deliverable. It is the *whole* of what the parent is handed when a child finishes
    /// between turns, so what it names and what it promises are asserted rather than left to the
    /// docstring — which is exactly the gap that let *"nothing fired"* be reported without anything
    /// in CI disagreeing about whether the notice could fire at all.
    ///
    /// **Two functions rather than a flag on one**, and the first assertion is why: the verb a child
    /// is read with is `task_result`, not `job_output`, so a model that gets the wrong one is being
    /// sent to a door that does not open. The second is that a child's completion carries an ANSWER
    /// where a job's carries a command's output, and the parent that is told only *it stopped* has to
    /// go and ask — which is the poll this whole path exists to end.
    #[test]
    fn a_childs_completion_names_the_child_and_points_at_task_result() {
        let one = JobCompletion {
            kind: BackgroundKind::Subagent,
            owner: "s-me".into(),
            job: "s-1-sub-1".into(),
            // A subagent runs no command, and the field must not quietly hold an answer.
            command: String::new(),
            state: "done".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: "two facts off a fixture".into(),
        };
        let text = subagent_notice(&[one.clone()], true);
        // The label, and it is what tells this row from a job's: one channel, two nouns.
        assert!(
            text.starts_with("[task]"),
            "labelled like a monitor: {text}"
        );
        assert!(
            text.contains("a subagent you started has finished"),
            "{text}"
        );
        assert!(text.contains("`s-1-sub-1`"), "{text}");
        assert!(text.contains("done"), "{text}");
        // **What the child SAID.** Without this the notice is a doorbell with no answer behind it:
        // the parent is told something happened and must still go and read.
        assert!(text.contains("two facts off a fixture"), "{text}");
        // The half that is the fix and not the footnote, in the job side's own words.
        assert!(text.contains("do not need to wait"), "{text}");
        assert!(text.contains("task_result"), "{text}");
        assert!(
            !text.contains("job_output"),
            "a subagent is read with `task_result`; sending the parent to `job_output` is a door \
             that does not open: {text}"
        );

        // **A child that said nothing gets no empty field.** The job side guards an empty command
        // (`` `` ``); the field that can be empty here is `detail`, and the shape it would leave is
        // a line ending in a colon with nothing after it.
        let bare = subagent_notice(
            &[JobCompletion {
                detail: String::new(),
                ..one.clone()
            }],
            true,
        );
        assert!(
            !bare.contains("done:"),
            "an empty detail drew a colon and nothing after it: {bare}"
        );
        assert!(bare.contains("`s-1-sub-1` done"), "{bare}");

        // **Two children are one notice, two lines, and the plural sentence** — a round that spawned
        // a pair reports a pair rather than making the reader count the list themselves.
        let pair = subagent_notice(
            &[
                one.clone(),
                JobCompletion {
                    job: "s-1-sub-2".into(),
                    ..one.clone()
                },
            ],
            true,
        );
        assert!(
            pair.contains("2 subagents you started have finished"),
            "{pair}"
        );
        assert!(
            pair.contains("s-1-sub-1") && pair.contains("s-1-sub-2"),
            "{pair}"
        );
    }
    /// **The wake's one decision, asserted: which sentence for which kind, and none for nothing.**
    ///
    /// The partition is the whole of what [`Harness::wake`] decides about a settlement — everything
    /// else it does is take the queue and submit the text — and it is asserted here because `wake()`
    /// itself cannot be: it needs a live `Harness`, which this crate's tests build only where a
    /// vocabulary and a store are present. See [`completion_notices`] for why that is not a reason to
    /// leave the decision unasserted.
    #[test]
    fn a_settlement_is_two_nouns_out_of_one_channel() {
        let job = |id: &str| JobCompletion {
            kind: BackgroundKind::Job,
            owner: "s-me".into(),
            job: id.into(),
            command: "cargo test".into(),
            state: "exited 0".into(),
            produced: 12,
            elapsed_ms: 900,
            detail: String::new(),
        };
        let child = |id: &str| JobCompletion {
            kind: BackgroundKind::Subagent,
            owner: "s-me".into(),
            job: id.into(),
            command: String::new(),
            state: "done".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: "off a fixture".into(),
        };

        // **Nothing settled says nothing**, which is what makes a spurious wake cost no generation.
        assert!(completion_notices(Vec::new(), "s-me").is_empty());

        // **The two nouns are the point.** A reader told `[job]` about a child goes looking for it
        // with `job_output` — which is the wrong door, and is what a hand measurement had to find
        // because nothing asserted this.
        let only_child = completion_notices(vec![child("s-1-sub-1")], "s-me");
        assert_eq!(only_child.len(), 1, "{only_child:?}");
        assert!(only_child[0].starts_with("[task]"), "{:?}", only_child[0]);
        assert!(only_child[0].contains("task_result"), "{:?}", only_child[0]);
        let only_job = completion_notices(vec![job("j7")], "s-me");
        assert_eq!(only_job.len(), 1, "{only_job:?}");
        assert!(only_job[0].starts_with("[job]"), "{:?}", only_job[0]);
        assert!(only_job[0].contains("job_output"), "{:?}", only_job[0]);

        // **Both kinds are one wake and two sentences**, job first — the order the old inline code
        // produced, kept so the record of a session reads the same way it did.
        let both = completion_notices(vec![job("j7"), child("s-1-sub-1")], "s-me");
        assert_eq!(both.len(), 2, "{both:?}");
        assert!(both[0].starts_with("[job]"), "{:?}", both[0]);
        assert!(both[1].starts_with("[task]"), "{:?}", both[1]);

        // **Two children are one sentence, not two** — the plural arm of the child's own wording,
        // asserted through the partition so the two functions cannot drift apart.
        let pair = completion_notices(vec![child("s-1-sub-1"), child("s-1-sub-2")], "s-me");
        assert_eq!(pair.len(), 1, "{pair:?}");
        assert!(
            pair[0].contains("2 subagents you started have finished"),
            "{:?}",
            pair[0]
        );
    }

    /// **A settlement handed up says WHOSE it was, and points at no door.**
    ///
    /// The other half of the ownership split, and the sentence the measured defect of
    /// 2026-10-05 needed: [`JobWatchers::stop`] hands a stopped session's undrained
    /// settlements to the level above, and this is what keeps that hand-over honest.
    ///
    /// The measurement, from the operator's own conversation: a subagent backgrounded
    /// `sleep 25; echo done`, and the notice arrived in the MAIN session's transcript
    /// reading *"a job you backgrounded has ended"* — while that session's own
    /// `job_output` answered *no job called `j17`*. A notice that says *you backgrounded*
    /// about a job you cannot read is not misplaced, it is false twice; and a notice that
    /// promised `job_output` to the session that cannot open it sends a model to a door
    /// that does not open, which is the rule this file keeps for `task_result` vs
    /// `job_output` one layer down.
    #[test]
    fn a_handed_up_settlement_names_its_owner_and_sends_nobody_to_a_shut_door() {
        let job = JobCompletion {
            kind: BackgroundKind::Job,
            owner: "s-child".into(),
            job: "j17".into(),
            command: "sleep 25; echo done".into(),
            state: "exited 0".into(),
            produced: 5,
            elapsed_ms: 25_000,
            detail: String::new(),
        };

        // The level it was handed up to — the ROOT — is not the session that started it.
        let theirs = completion_notices(vec![job.clone()], "s-root");
        assert_eq!(theirs.len(), 1, "{theirs:?}");
        assert!(
            !theirs[0].contains("you backgrounded"),
            "the one sentence that must never be said about somebody else's job: {:?}",
            theirs[0]
        );
        assert!(
            theirs[0].contains("a job a session below this one started has ended"),
            "{:?}",
            theirs[0]
        );
        assert!(
            theirs[0].contains("started by `s-child`"),
            "the owner is named on the fact line: {:?}",
            theirs[0]
        );
        assert!(
            !theirs[0].contains("job_output"),
            "its host went with the session that started it, so `job_output` here is a door \
             that does not open: {:?}",
            theirs[0]
        );

        // **The same settlement, read by the session that started it** — which is the
        // sentence that was right all along and must not have moved.
        let mine = completion_notices(vec![job.clone()], "s-child");
        assert!(
            mine[0].contains("a job you backgrounded has ended"),
            "{:?}",
            mine[0]
        );
        assert!(mine[0].contains("job_output"), "{:?}", mine[0]);
        assert!(!mine[0].contains("started by"), "{:?}", mine[0]);

        // **Its own come first**, so one drain reads as this session's own news before what
        // it is being told on somebody else's behalf — two owners, two notices, that order.
        let mine_job = JobCompletion {
            owner: "s-root".into(),
            job: "j3".into(),
            ..job.clone()
        };
        let both = completion_notices(vec![job, mine_job], "s-root");
        assert_eq!(both.len(), 2, "{both:?}");
        assert!(
            both[0].contains("a job you backgrounded has ended") && both[0].contains("`j3`"),
            "{:?}",
            both[0]
        );
        assert!(
            both[1].contains("started by `s-child`") && both[1].contains("`j17`"),
            "{:?}",
            both[1]
        );

        // A handed-up GRANDCHILD keeps its own door: `task_result` reads the tree's handle
        // list (`Parts::tree_slots`), so unlike a dead job's output the answer is still there.
        let child = JobCompletion {
            kind: BackgroundKind::Subagent,
            owner: "s-child".into(),
            job: "s-child-sub-1".into(),
            command: String::new(),
            state: "done".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: "the grandchild answered".into(),
        };
        let handed = completion_notices(vec![child], "s-root");
        assert_eq!(handed.len(), 1, "{handed:?}");
        assert!(
            handed[0].contains("a subagent a session below this one started has finished"),
            "{:?}",
            handed[0]
        );
        assert!(
            handed[0].contains("started by `s-child`"),
            "{:?}",
            handed[0]
        );
        assert!(
            handed[0].contains("the grandchild answered"),
            "{:?}",
            handed[0]
        );
        assert!(
            handed[0].contains("task_result"),
            "the handle is in this tree, so this door does open: {:?}",
            handed[0]
        );
    }

    /// **A finished foreground command is not a job anybody acts on.**
    ///
    /// The operator ran `/job` after a turn and got hundreds of lines — one per
    /// `grep`, `cat` and `sed` the model had run all session — because the host
    /// gives every command a job id and the listing showed all of them. Measured
    /// in their own store the same day: of 1,210 tool results in that session,
    /// exactly **2** were `backgrounded`.
    #[test]
    fn only_background_and_still_running_jobs_are_listed() {
        use letibot_tools::exec::{JobId, JobState, JobView};
        use letibot_tools::exec::{ScopeId, ScopeKind};
        let scope = || ScopeId {
            kind: ScopeKind::Session,
            name: "s".into(),
            path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
        };
        let job = |background, state| JobView {
            id: JobId("j1".into()),
            command: "grep -n x y".into(),
            scope: scope(),
            owner: scope(),
            cwd: "/".into(),
            pid: 1,
            background,
            state,
            elapsed: std::time::Duration::from_secs(1),
            ran_for: Some(std::time::Duration::from_secs(1)),
            produced: 91,
            since_last_output: None,
        };

        // The case that flooded the screen: finished, never backgrounded.
        assert!(!Harness::worth_listing(&job(
            None,
            JobState::Exited { code: 0 }
        )));
        // A failure is still not a job — the tool result already carried it.
        assert!(!Harness::worth_listing(&job(
            None,
            JobState::Exited { code: 1 }
        )));
        // Still running, never backgrounded: ctrl-o moves this one, so it stays.
        assert!(Harness::worth_listing(&job(None, JobState::Running)));
        // Backgrounded, however it got there, finished or not.
        for how in [
            letibot_transcript::Backgrounding::Asked,
            letibot_transcript::Backgrounding::Promoted,
        ] {
            assert!(Harness::worth_listing(&job(
                Some(how.clone()),
                JobState::Running
            )));
            assert!(Harness::worth_listing(&job(
                Some(how),
                JobState::Exited { code: 0 }
            )));
        }
    }

    /// **A row is one line, whatever the model wrote.**
    ///
    /// A heredoc carries newlines inside the command text, and the listing cut it
    /// to 60 *characters* — keeping the newline, so one row drew as two and a
    /// screen of them was unreadable. The operator, 2026-09-20, looking at it:
    /// *"some fuckery with jobs"*.
    #[test]
    fn a_job_row_is_one_line_however_many_the_command_had() {
        let heredoc = "cd /home/dead/Projects/leticl && python3 - <<'PY'\ns = open(\"x\")\nPY";
        let line = Harness::one_line(heredoc, 60);
        assert!(!line.contains('\n'), "a row that draws as two: {line:?}");
        // 60 characters plus the ellipsis that says something was cut.
        assert_eq!(line.chars().count(), 61, "{line:?}");
        assert!(line.ends_with('…'), "{line:?}");
        assert!(
            line.starts_with("cd /home/dead/Projects/leticl && python3"),
            "{line:?}"
        );

        // A short command is left exactly as it is — no ellipsis promising more.
        assert_eq!(Harness::one_line("git status", 60), "git status");
        // Internal runs of whitespace collapse, so the width asked for is the
        // width drawn.
        assert_eq!(Harness::one_line("ls   -la\t-h", 60), "ls -la -h");
    }

    /// **The url is written the way it is curled.** A scheme and the chat-completions
    /// path both appear in what the operator pastes, and both have to come off: the
    /// client appends its own route, so a kept path would post to
    /// `/v1/chat/completions/completion`.
    #[test]
    fn a_fleet_url_is_read_the_way_the_operator_writes_it() {
        let e = super::parse_local_url("http://192.168.1.78:8082/v1/chat/completions")
            .expect("a curl's url");
        assert_eq!(e.authority(), "192.168.1.78:8082");
        assert_eq!(
            super::parse_local_url("192.168.1.78:8082")
                .unwrap()
                .authority(),
            "192.168.1.78:8082",
            "a bare authority is the same address"
        );
        assert_eq!(
            super::parse_local_url("https://box.lan:443/")
                .unwrap()
                .authority(),
            "box.lan:443"
        );
    }

    /// A port is not optional and the refusal says so, because the alternative is
    /// guessing 8080 — which is the port the three LOCAL units share, so the guess
    /// would land on this box while the operator was naming another one.
    #[test]
    fn a_fleet_url_without_a_port_is_refused_by_name() {
        let e = super::parse_local_url("http://192.168.1.78").unwrap_err();
        assert!(e.contains("no port"), "{e}");
        assert!(super::parse_local_url("http://").is_err());
    }

    /// The GGUF's file name is the comparison, so two boxes holding one model in
    /// different directories agree, and a split file's shard count still has to match.
    #[test]
    fn the_vocab_comparison_is_the_file_name_not_the_path() {
        use std::path::Path;
        assert_eq!(
            super::vocab_basename(Path::new("/home/dead/models/Qwen3.8-27B-UD-Q6_K_XL.gguf")),
            super::vocab_basename(Path::new("/srv/weights/Qwen3.8-27B-UD-Q6_K_XL.gguf")),
            "one model in two places is one model"
        );
        assert_ne!(
            super::vocab_basename(Path::new("/m/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf")),
            super::vocab_basename(Path::new("/m/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00008.gguf")),
            "a different shard count is a different file"
        );
    }

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text { text: text.into() }],
        }
    }

    /// A `User` row **this session appended** — a job settlement, a salvage notice,
    /// an intent check. The same variant on the wire and never the operator's words.
    fn agent_row(text: &str) -> TranscriptItem {
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text: text.into() }],
        }
    }

    /// A ledger with an encoder attached, which is what every session has.
    ///
    /// Constructing the decorator is the *only* way to say so — the flag is
    /// `pub(crate)` in `letibot-tools` precisely so that "an encoder was declared"
    /// and "an encoder was wired" cannot be two different facts. The sink is
    /// dropped; the attachment is not.
    fn encoded() -> IntentLedger {
        let l = IntentLedger::new();
        let l = Arc::new(l);
        drop(IntentSink::new(l.clone(), letibot_tools::NullToolSink));
        Arc::try_unwrap(l).expect("the sink was just dropped")
    }

    #[test]
    fn a_downgrade_denies_the_seated_tools_of_its_classes_and_the_well_known_names() {
        use letibot_tools::permission::{Action, Rule};
        use letibot_tools::schema::{Access, Downgrade, ToolSchema};
        let seated = vec![
            ToolSchema::new("read", "r", serde_json::json!({}), Access::Read),
            ToolSchema::new("edit", "e", serde_json::json!({}), Access::Write),
            ToolSchema::new("bash", "b", serde_json::json!({}), Access::Exec),
            ToolSchema::new("flowy", "f", serde_json::json!({}), Access::Network),
            ToolSchema::new("odd_writer", "o", serde_json::json!({}), Access::Write),
        ];
        let parent = vec![Rule::new("edit", "*", Action::Allow)];
        let rules = downgraded_ruleset(&parent, &Downgrade::parse("no-write").unwrap(), &seated);
        let denied: Vec<&str> = rules
            .iter()
            .filter(|r| r.action == Action::Deny)
            .map(|r| r.permission.as_str())
            .collect();
        assert!(
            denied.contains(&"edit") && denied.contains(&"write") && denied.contains(&"odd_writer"),
            "{denied:?}"
        );
        assert!(
            !denied.contains(&"read") && !denied.contains(&"bash") && !denied.contains(&"flowy"),
            "{denied:?}"
        );
        // Last rule wins: the parent's allow for `edit` is overridden.
        let r = letibot_tools::permission::evaluate("edit", "anything", &[&rules]);
        assert_eq!(r.action, Action::Deny);
        // No downgrade, no change.
        assert_eq!(
            downgraded_ruleset(&parent, &Downgrade::none(), &seated),
            parent
        );
    }

    /// **A child's answer is settled once, and a path out with no answer still settles.**
    ///
    /// Two failures this guard exists for, and both are silent without it: a settlement that
    /// never fires leaves the parent's `task_result` waiting for ever, and a second one lets a
    /// path out of the call overwrite the answer the child actually gave.
    #[test]
    fn a_childs_answer_is_settled_once_and_never_overwritten() {
        use letibot_tools::builtins::task::TaskStatus;

        // The ordinary ending.
        let slot = TaskSlot::new("s-parent");
        {
            let answer = AnswerOnce::new(&slot);
            answer.say(TaskStatus::Done {
                answer: "the child's answer".into(),
            });
        }
        assert_eq!(
            slot.status(),
            TaskStatus::Done {
                answer: "the child's answer".into()
            }
        );

        // **A path out with no answer still settles** — the one that would hang a parent: a
        // thread ending with the slot still `Running` is a `task_result` that never returns.
        let slot = TaskSlot::new("s-parent");
        {
            let _answer = AnswerOnce::new(&slot);
        }
        assert!(
            matches!(slot.status(), TaskStatus::Failed { .. }),
            "an unsettled child stayed `running` for ever: {:?}",
            slot.status()
        );

        // **And the first answer is the answer.**
        let slot = TaskSlot::new("s-parent");
        {
            let answer = AnswerOnce::new(&slot);
            answer.say(TaskStatus::Done {
                answer: "said".into(),
            });
            answer.say(TaskStatus::Failed {
                why: "the park ended oddly".into(),
            });
        }
        assert_eq!(
            slot.status(),
            TaskStatus::Done {
                answer: "said".into()
            },
            "a later path out overwrote what the child said"
        );
    }

    /// **A prompt is answered, a stop is a STOP for the whole subtree, and everything else a
    /// child inherits is left, and named.**
    ///
    /// The operator's ruling is what makes the first arm right — *"subagent is just another
    /// agent… mid turn, post turn whatever"* — and the other arms are what keeps this door from
    /// running the parent's machinery in the parent's name.
    ///
    /// **The interrupt arm is the correction, and it is the measured one.** It used to say
    /// *"nothing is running to interrupt"*, which was true of the session and false of its
    /// subtree: a child between turns, with children of its own still computing, was told
    /// nothing and left them running. See the supervision invariant on [`HarnessTaskRunner`].
    #[test]
    fn a_child_answers_a_prompt_and_leaves_the_machinery_its_parent_owns() {
        use letibot_sessionlog::CommandKind as K;
        assert_eq!(
            child_command(&K::Prompt {
                text: "do this next".into()
            }),
            ChildCommand::Answer,
            "the operator asking a child something is a turn, not a no-op"
        );
        assert_eq!(
            child_command(&K::Interrupt {
                reason: "esc".into()
            }),
            ChildCommand::Stop,
            "an interrupt IS this child's stop between turns — its children go first, then it does"
        );
        for (kind, why) in [
            (
                K::Message {
                    from: "s-parent".into(),
                    text: "change of plan".into(),
                },
                "a message steers a turn in flight",
            ),
            (K::Compact, "a door a child does not own"),
            (
                K::Mode {
                    name: "allow-all".into(),
                    consented: true,
                },
                "a door a child does not own",
            ),
        ] {
            assert_eq!(child_command(&kind), ChildCommand::Leave(why), "{kind:?}");
        }
    }

    /// **A progress bar is ONE line, and the window must show its last state.**
    ///
    /// The operator's report, 2026-10-05: *"more often than not i'd like to enter the job and
    /// hope to tail its out with ascii codes correctly processed"*. `cargo`, `docker` and `npm`
    /// redraw a single row with `\r` and no newline in it, so splitting on `\n` alone put every
    /// update on one row — `2%23%47%100%` — which is what they were reading.
    #[test]
    fn a_progress_bar_is_one_line_and_not_every_update_concatenated() {
        // The ordinary case: each redraw is longer, so the last one stands alone.
        assert_eq!(
            progress_lines("  2%\r 23%\r 47%\r100%"),
            vec!["100%"],
            "the row is what a terminal would end up showing"
        );
        // **A SHORTER final segment leaves the tail of the longer one standing** — that is
        // what the screen showed, and dropping the tail would be inventing a different one.
        // (The naive `after the last \r` rule gets this wrong.)
        assert_eq!(progress_lines("abcdefgh\rXY"), vec!["XYcdefgh"]);
        // Newlines still split; a carriage return at the end of a line ends it rather than
        // being kept as a byte nobody can see.
        assert_eq!(progress_lines("a\nb\rc\nd\r"), vec!["a", "c", "d"]);
        // And it is not a rewrite of ordinary output: no CR, no change.
        assert_eq!(
            progress_lines("$ cargo test\nrunning 3 tests"),
            vec!["$ cargo test", "running 3 tests"]
        );
    }

    /// **A parent's correction reaches a running child as steering, and NOT as the
    /// operator's words** — the operator's ruling, 2026-10-06: *"in the tree all subagents
    /// must be addressable by their parents. that is how live corrections delivered."*
    ///
    /// The path, end to end and without a daemon: the runner submits under the **daemon's**
    /// name (the parent is seated in another session's hub), the kind carries the parent's
    /// identity, and the child's own steering poll takes it at its next round boundary. What
    /// this pins is the pair of facts a correction cannot afford to get wrong — it is not
    /// `from_operator`, so it cannot authorise the act it races, coalesce with the operator's
    /// text, or be taken back by a recall; and it is not `urgent`, so a child told to change
    /// course finishes the call it is in rather than abandoning it.
    ///
    /// The parent is named in the text because the trail cannot name it: `say` takes a
    /// speaker kind, not an identity, and *which* parent spoke is the whole of what
    /// addressability means.
    #[test]
    fn a_parents_message_is_the_childs_steering_and_not_the_operators_words() {
        let hub = Hub::new("s-child");
        let mut steering = HubSteering {
            hub: hub.clone(),
            // No trail: what is asserted is the message the child reads and how it is
            // scoped, and a trail would only add a second place for the same words.
            trail: None,
            injected: Arc::new(Mutex::new(VecDeque::new())),
            monitors: None,
            monitor_cursor: Arc::new(AtomicUsize::new(0)),
            job_watch: None,
            // No children either: what is asserted here is the message's scoping.
            subagents: None,
            held: VecDeque::new(),
        };

        // **What `HarnessTaskRunner::send` does**, exactly: the daemon's own name at the
        // door, the parent's id in the kind.
        let f = hub.submit(
            letibot_sessionlog::hub::DAEMON_SUBMITTER,
            "task_message-1",
            0,
            letibot_sessionlog::CommandKind::Message {
                from: "s-parent".into(),
                text: "stop and report what you have".into(),
            },
        );
        assert!(
            matches!(f, letibot_sessionlog::ServerFrame::Accepted { .. }),
            "the child's hub refused the daemon's relay: {f:?}"
        );

        let got = steering
            .try_next()
            .expect("the child's next round boundary takes it");
        assert!(
            !got.from_operator,
            "a parent's correction was recorded as the operator's own words: {got:?}"
        );
        assert!(
            !got.urgent,
            "a correction is context for the next round, not an interrupt: {got:?}"
        );
        assert!(
            got.text.contains("s-parent") && got.text.contains("stop and report what you have"),
            "the child cannot tell which parent spoke, or what it said: {}",
            got.text
        );
        // Taken, not peeked: a correction delivered twice is a child told the same thing
        // at two round boundaries, which reads as insistence.
        assert!(
            steering.try_next().is_none(),
            "the same message was handed over twice"
        );
    }

    /// **A failed turn's words come back out of the hub's steering, still the
    /// operator's, and not twice.**
    ///
    /// The hub half of the fix for the measured loss: a message is out of the hub's
    /// queue the moment `try_next` dequeues it, and a turn that fails commits nothing
    /// (§5.7), so the words go back to the source rather than being appended.
    /// `give_back` holds them, and the next `try_next` drains them before the hub's
    /// queue — with the flags they had when they were taken, because it is the same
    /// `SteeringMessage` coming out again.
    #[test]
    fn a_handed_back_message_comes_out_of_the_next_poll_still_the_operators() {
        let hub = Hub::new("s-steer");
        let mut steering = HubSteering {
            hub: hub.clone(),
            trail: None,
            injected: Arc::new(Mutex::new(VecDeque::new())),
            monitors: None,
            monitor_cursor: Arc::new(AtomicUsize::new(0)),
            job_watch: None,
            subagents: None,
            held: VecDeque::new(),
        };

        // An interrupt queued behind the failed turn: it must come out AFTER the
        // handed-back words, which were taken before it. (The daemon may submit an
        // interrupt and nothing else that speaks as a person, so this is the door
        // the test goes through.)
        let f = hub.submit(
            letibot_sessionlog::hub::DAEMON_SUBMITTER,
            "interrupt-1",
            0,
            letibot_sessionlog::CommandKind::Interrupt {
                reason: "ABORT".into(),
            },
        );
        assert!(
            matches!(f, letibot_sessionlog::ServerFrame::Accepted { .. }),
            "the hub refused the interrupt: {f:?}"
        );

        // The failed turn hands the operator's words back.
        steering.give_back(vec![SteeringMessage::operator(
            "use web_search and fetch to lookup git commands meaning",
        )]);

        // The next poll takes the handed-back words first, still the operator's.
        let back = steering
            .try_next()
            .expect("the handed-back words come out of the next poll");
        assert_eq!(
            back.text,
            "use web_search and fetch to lookup git commands meaning"
        );
        assert!(
            back.from_operator,
            "the words came out as somebody else's: {back:?}"
        );

        // Not handed over twice: the next poll takes the queued interrupt, then nothing.
        let next = steering
            .try_next()
            .expect("the queued interrupt still comes out");
        assert!(
            next.urgent,
            "the interrupt came out as something else: {next:?}"
        );
        assert!(
            steering.try_next().is_none(),
            "a message was handed over twice"
        );
    }

    #[test]
    fn flowy_is_a_door_for_every_root_session_and_for_no_subagent() {
        let mut cfg = Config::for_this_box(std::env::temp_dir());
        cfg.seat = Seat::Leticode;
        cfg.allow_bash = true;
        // Seat or no seat: the door is there, so `/flowy login` can fill it.
        let without = role_for(&cfg);
        assert!(without.tools.iter().any(|t| t == "flowy"));

        cfg.flowy = Some(crate::config::FlowyConfig::default());
        let root = role_for(&cfg);
        assert!(root.tools.iter().any(|t| t == "flowy"));
        // The whole opencode union plus the room fits the ceiling, which moved to
        // twenty-four when `task_message` joined the delegation trio — see leticode's own
        // note on `max_tools` for why it took a seat rather than trading for one. A tool
        // added after this has to do the same: take a seat from something, or declare its
        // own number where somebody deciding can read it.
        assert!(
            root.tools.len() <= root.max_tools,
            "{} > {}",
            root.tools.len(),
            root.max_tools
        );

        // A subagent hears through its parent.
        cfg.parent_session_id = Some("parent".into());
        assert!(!role_for(&cfg).tools.iter().any(|t| t == "flowy"));

        // The runner has no spare seat, and the disclosure says NOT SEATED.
        cfg.parent_session_id = None;
        cfg.seat = Seat::Runner;
        let runner = role_for(&cfg);
        assert!(!runner.tools.iter().any(|t| t == "flowy"));
        assert!(runner.tools.len() <= runner.max_tools);
    }

    #[test]
    fn the_shell_is_declared_pinned_only_where_an_exec_backend_exists() {
        // R10's wiring. The pin claim rides on the host spawn's env hygiene —
        // `a_bash_env_planted_in_the_parent_never_reaches_the_child` in
        // `letibot-tools` holds that — so a seat with an exec backend declares
        // it, and a seat without one leaves `Unknown`, under which a bare name
        // is `not_run`: the fail-closed direction.
        let mut cfg = Config::for_this_box(std::env::temp_dir());
        cfg.seat = Seat::Runner;
        match surroundings_for(&cfg).shell {
            letibot_tools::ShellTrust::Pinned { how } => {
                assert!(how.contains("cleared before its own pairs"), "{how}");
            }
            other => panic!("a runner seat must declare the pin, got {other:?}"),
        }
        cfg.seat = Seat::Orchestrator;
        assert_eq!(
            surroundings_for(&cfg).shell,
            letibot_tools::ShellTrust::Unknown
        );
    }

    /// **R18's daemon half: layer A is given the configured workspace.**
    ///
    /// The wrong fact the operator's four cards showed came from the *backend's root*
    /// (`/` for an unconfined seat), not from here — `surroundings_for` has always used
    /// `cfg.workspace`. This pins it, because two halves of one program answering *where
    /// is this session* differently is exactly the shape of that defect, and nothing in
    /// either half would have noticed.
    #[test]
    fn the_gate_is_told_the_configured_workspace_and_not_a_root() {
        let mut cfg = Config::for_this_box("/home/dead/Projects/letibot");
        assert_eq!(
            surroundings_for(&cfg).workspace.as_deref(),
            Some("/home/dead/Projects/letibot")
        );
        // …and it follows `--workspace` rather than a constant, which is the property
        // that makes the configured value the one that reaches the classifier.
        cfg.workspace = std::path::PathBuf::from("/tmp/somewhere-else");
        assert_eq!(
            surroundings_for(&cfg).workspace.as_deref(),
            Some("/tmp/somewhere-else")
        );
    }

    /// **The stop must not blame the model for working.**
    ///
    /// The sentence this replaced was *"the model called tools 12 times without
    /// answering"*. It was answering; it had not finished, and an operator who reads
    /// that learns to distrust the model when the harness was at fault — the same
    /// defect as `grep` reporting absence when it had opened no files.
    #[test]
    fn neither_stop_accuses_the_model_of_not_answering() {
        let backstop = HarnessError::LoopBound { rounds: 200 }.to_string();
        assert!(
            !backstop.contains("without answering"),
            "the accusation is the defect: {backstop}"
        );
        assert!(
            backstop.contains("--max-tool-rounds"),
            "errors carry the fix: {backstop}"
        );

        // The progress stop says what it SAW. Built here the way the loop builds it,
        // so the wiring and the sentence cannot drift apart.
        let mut d = crate::progress::ProgressDetector::new(2);
        for _ in 0..3 {
            d.observe(
                &ToolCall {
                    id: "c0".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"src/main.rs"}"#.into(),
                },
                &letibot_transcript::ToolOutcome::Ok,
                "the same body",
            );
            d.end_round();
        }
        assert!(d.exhausted());
        let e = HarnessError::NoProgress {
            evidence: d.evidence(9, 200),
        }
        .to_string();
        assert!(e.contains("src/main.rs"), "it names the evidence: {e}");
        assert!(e.contains("0 of 2"), "with its denominator: {e}");
        assert!(!e.contains("without answering"), "{e}");
    }

    /// **The turn boundary carries the intent finding and NOTHING about the plan.**
    ///
    /// This test used to assert the opposite — that the plan's finding was joined in at the turn
    /// boundary because *"joining is what keeps this inside the one-nudge budget"*. That budget was
    /// the thing the operator rejected: one nudge per user turn means a session in constant
    /// conversation is checked after every exchange (*"looks like our todo nag is overkill"*) while
    /// a quiet one is checked once and never again. The plan is now delivered by the IDLE clock
    /// (`Sessions::rearm_todo_nag`, `Harness::nag_turn`), so what is left here is the finding that
    /// really is about the turn that just ended.
    ///
    /// The plan half has its own tests where it now lives: `unfinished_plan` in `letibot-tools` for
    /// the text, and `nag_should_arm` below for the schedule.
    #[test]
    fn the_turn_boundary_says_what_the_turn_promised_and_not_what_the_plan_is() {
        // A session with nothing declared has nothing to say at the boundary — the common case,
        // and the one that must stay silent.
        assert!(
            steer_for_turn(&encoded(), false, "t1", &[]).is_none(),
            "an encoded ledger with nothing declared has no boundary finding"
        );
        // And the finding that IS the boundary's own: a turn that declared something and did
        // nothing to show for it. It carries no plan text at all, which is the whole claim of
        // this test — the two checks are no longer one message.
        let bare = IntentLedger::new();
        let steer = steer_for_turn(&bare, false, "t1", &[])
            .expect("no encoder is itself a finding, on every turn");
        assert!(!steer.contains("[todo check]"), "{steer}");
    }

    /// **An unattached encoder is loud, not silent.** The state this is asserting
    /// is the one a session must never be in quietly: a diff computed against
    /// nothing would report every completion as verified, which is exactly the
    /// announced-but-not-done failure wearing the check's own clothes.
    #[test]
    fn a_ledger_with_no_encoder_says_so_in_the_diff_itself() {
        let bare = IntentLedger::new();
        let steer = steer_for_turn(&bare, false, "t1", &[])
            .expect("no encoder is itself a finding, on every turn");
        assert!(steer.contains("effect log"), "{steer}");
        assert!(
            steer_for_turn(&encoded(), false, "t1", &[]).is_none(),
            "an encoded ledger with nothing declared has nothing to say"
        );
    }

    // -- the authorisation trail --------------------------------------------

    /// **The harness must not be able to authorise itself.**
    ///
    /// Everything a session appends as a `User` item looks identical in the
    /// transcript: the operator's prompt, a head's steering, §5.7's salvage notice
    /// and the intent check's own *"you said you would X"*. A trail walked over the
    /// transcript would read all four as the operator's words, and an agent whose
    /// own text can authorise an action is the shape a prompt injection would most
    /// like to take. This is the assertion that keeps the four apart.
    #[test]
    fn an_injected_notice_is_never_read_back_as_the_operator() {
        let m = TrailMirror::default();
        m.begin_turn();
        m.say(
            Speaker::Operator,
            "ssh to lubuntu2 and check the build",
            None,
        );
        m.say(
            Speaker::Agent,
            "[intent check] you said you would ssh",
            None,
        );

        let t = m.trail();
        let words: Vec<&str> = t.operator_words().iter().map(|u| u.text.as_str()).collect();
        assert_eq!(words, vec!["ssh to lubuntu2 and check the build"]);
        assert_eq!(
            t.utterances.len(),
            2,
            "both are carried, with their speakers"
        );
        assert!(
            t.utterances.iter().any(|u| u.speaker == Speaker::Agent),
            "the agent's own line is carried as context and never as authority"
        );
    }

    /// The denominator is what was **looked at**, and it is not derived from what
    /// came back.
    ///
    /// `docs/tool-design-brief.md` §2.2 applied to a security input: an adjudicator
    /// told *"the operator said nothing"* when nobody looked would deny the thing
    /// that was asked for. `0 of 41` is a measurement; `0` is not.
    #[test]
    fn a_trail_with_no_operator_words_still_carries_its_denominator() {
        let m = TrailMirror::default();
        m.note_items(41);
        let t = m.trail();
        assert!(
            t.was_collected(),
            "somebody looked; this is not `NotCollected`"
        );
        assert!(t.operator_words().is_empty());
        match t.provenance {
            TrailProvenance::Scanned {
                messages_scanned,
                operator_messages,
            } => {
                assert_eq!(operator_messages, 0);
                assert!(
                    messages_scanned > 0,
                    "a scan of 41 items reported a denominator of {messages_scanned}"
                );
            }
            TrailProvenance::NotCollected { .. } => panic!("something looked"),
        }
    }

    /// A gate with no trail source installed reports `NotCollected` — **not** an
    /// empty trail. This is the state the whole seam exists to leave behind, and it
    /// is asserted here so that "the default is safe" is a fact rather than a
    /// recollection.
    #[test]
    fn an_uninstalled_trail_is_not_collected_rather_than_empty() {
        let t = AuthorisationTrail::default();
        assert!(!t.was_collected());
        assert!(t.render().contains("NOT COLLECTED"), "{}", t.render());
    }

    /// Distance is half the evidence: a *"yeah restart"* from six turns ago is not
    /// the same fact as one from this turn, and a trail that dropped the distance
    /// would present them as identical.
    #[test]
    fn recency_is_carried_and_the_clock_is_absent_rather_than_invented() {
        let m = TrailMirror::default();
        m.begin_turn();
        m.say(Speaker::Operator, "old", None);
        for _ in 0..5 {
            m.begin_turn();
        }
        m.say(Speaker::Operator, "recent", Some(Instant::now()));

        let t = m.trail();
        let by = |s: &str| {
            t.utterances
                .iter()
                .find(|u| u.text == s)
                .unwrap_or_else(|| panic!("{s} is missing"))
        };
        assert_eq!(by("recent").turns_ago, 0);
        assert_eq!(by("old").turns_ago, 5);
        // `None` reads as *not recorded*, never as *just now* — which is why the
        // rebuilt-from-store rows keep it.
        assert_eq!(by("old").seconds_ago, None);
        assert!(by("recent").seconds_ago.is_some());
    }

    /// A rebuilt transcript seeds the trail, and the clock does not come back with it.
    /// Asserted so that the resume note beside it is describing something true.
    #[test]
    fn a_resumed_trail_is_seeded_with_no_clock() {
        let m = TrailMirror::default();
        m.seed(&[
            user("first"),
            TranscriptItem::Assistant {
                text: "an answer".into(),
                tool_calls: vec![],
                truncated: false,
            },
            user("second"),
        ]);
        let t = m.trail();
        assert_eq!(t.operator_words().len(), 2);
        assert!(
            t.utterances.iter().all(|u| u.seconds_ago.is_none()),
            "nothing recorded when a stored row was said; a reconstructed clock \
             would be a guess presented as a measurement"
        );
    }

    /// **A resumed session does not promote its own words to the operator's.**
    ///
    /// The regression this closes was live and measurable: `seed` destructured
    /// `TranscriptItem::User { parts, .. }` and stamped `Speaker::Operator` on every
    /// row, so a job settlement came back as an operator utterance and
    /// [`AuthorisationTrail::cited_operator_words`] — which exists precisely to refuse
    /// an ALLOW citing agent text — had nothing left to refuse. 280 of 715 briefs the
    /// oracle answered carried one.
    ///
    /// Both halves are asserted, because only one of them is the bug: the agent row is
    /// still **carried** (it is context, and `Speaker::Agent` says what it is), and it
    /// is no longer **counted** as something that could authorise anything.
    #[test]
    fn a_resumed_trail_does_not_call_the_harnesss_own_rows_the_operators() {
        let m = TrailMirror::default();
        m.seed(&[
            user("fix the compaction"),
            agent_row("[job] a job you backgrounded has ended:\n  - `j6` exited 0"),
            user("restarted it"),
        ]);
        let t = m.trail();

        assert_eq!(
            t.utterances.len(),
            3,
            "the agent row is carried — it is context the operator's reply may lean on"
        );
        assert_eq!(
            t.operator_words().len(),
            2,
            "but it is not one of the operator's words"
        );

        // The citation check is the thing that was defeated, so assert it directly
        // rather than trusting the count above to stand in for it.
        let job = t
            .utterances
            .iter()
            .position(|u| u.text.starts_with("[job]"))
            .expect("the job row is on the page");
        assert_eq!(
            t.cited_operator_words(&[job]),
            Vec::<usize>::new(),
            "an ALLOW citing the harness's own job notice authorises nothing"
        );
        assert_eq!(
            t.cited_operator_words(&[0, job, 2]),
            vec![0, 2],
            "the operator's own lines still survive beside it"
        );
    }

    // -- the intent diff -----------------------------------------------------

    /// **T21.3, closed.** An item declared in a turn that then ran nothing is the
    /// acceptance case, and the steering text is what the model is handed.
    #[test]
    fn an_intent_declared_and_not_acted_on_produces_steering() {
        let l = encoded();
        l.declare("t1", "restart the model server", intent_tools::Source::Todo);
        let steer = steer_for_turn(&l, false, "t1", &[]).expect("a declared item and no effect");
        assert!(steer.contains("intent check"), "{steer}");
        assert!(steer.contains("restart the model server"), "{steer}");
    }

    /// **The default does not fire on prose**, which is the whole reason the prose
    /// half is a flag. A turn that says "I'll check the file" and calls nothing is
    /// the *normal* shape of a plain answer, and nudging it would put the check into
    /// conversations that were working.
    #[test]
    fn the_prose_heuristic_is_off_unless_it_is_asked_for() {
        let l = encoded();
        let items = [TranscriptItem::Assistant {
            text: "I'll check the file and get back to you.".into(),
            tool_calls: vec![],
            truncated: false,
        }];
        assert_eq!(
            steer_for_turn(&l, false, "t1", &items),
            None,
            "the deterministic half must not read prose"
        );
        let with_prose = steer_for_turn(&l, true, "t2", &items)
            .expect("the prose half is what --intent-prose buys");
        assert!(with_prose.contains("check the file"), "{with_prose}");
    }

    /// The diff is idempotent per turn. A turn boundary that fires twice is a thing
    /// that happens, and a diff that grows each time it is read is not a
    /// measurement.
    #[test]
    fn reading_the_same_turn_twice_says_the_same_thing() {
        let l = encoded();
        l.declare("t1", "write the report", intent_tools::Source::Todo);
        let a = steer_for_turn(&l, false, "t1", &[]);
        let b = steer_for_turn(&l, false, "t1", &[]);
        assert_eq!(a, b);
    }

    // -- the monitor notice --------------------------------------------------

    /// A monitor that expired is **not** a monitor that fired, and the sentence the
    /// model gets has to keep them apart — reporting a deadline as a completion is
    /// F5 one primitive over.
    #[test]
    fn a_monitor_notice_says_which_of_the_four_endings_it_was() {
        let monitors = Monitors::new();
        assert_eq!(monitors.settled_count(), 0);
        // Nothing declared, so nothing settles: the notice builder is exercised
        // through the empty case here and through the real one in `background.rs`,
        // which owns the four endings. What matters at this layer is that the
        // sentence carries `word()` rather than a boolean.
        let text = monitor_notice(&[]);
        assert!(text.contains("FIRED"), "{text}");
        assert!(
            text.contains("expiry") || text.contains("stopped existing"),
            "the notice must distinguish a firing from a watch that merely ended: {text}"
        );
    }
}

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
    use super::{
        MAX_HTTP_RETRIES, SilenceWatch, http_retry_after, retry_host, silence_host, silence_notice,
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
        use super::*;
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

    #[test]
    fn a_credential_the_server_refuses_is_not_retried() {
        assert!(http_retry_after(&status(401), 0, MAX_HTTP_RETRIES).is_none());
        assert!(http_retry_after(&status(403), 0, MAX_HTTP_RETRIES).is_none());
        // And not on a later attempt either — it is the code, not the streak.
        assert!(http_retry_after(&status(401), 3, MAX_HTTP_RETRIES).is_none());
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

    /// The case this exists for: llama.cpp reloading a six-shard GGUF after
    /// `--sleep-idle-seconds`, answering `503 Loading model` until it is up.
    /// Seen repeatedly on this box on 2026-09-17.
    #[test]
    fn a_server_that_is_coming_back_up_is_waited_out() {
        for code in [500, 502, 503, 504, 429] {
            assert!(
                http_retry_after(&status(code), 0, MAX_HTTP_RETRIES).is_some(),
                "{code} should be waited out"
            );
        }

        // No answer at all, and a body that did not parse — both likeliest to be
        // a server going down mid-answer.
        assert!(
            http_retry_after(
                &HttpError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "no"
                )),
                0,
                MAX_HTTP_RETRIES
            )
            .is_some()
        );
        assert!(
            http_retry_after(
                &HttpError::Malformed("truncated".into()),
                0,
                MAX_HTTP_RETRIES
            )
            .is_some()
        );
    }

    /// **A 429 that names money is not taken again** — MEASURED 2026-10-04, the operator's GLM
    /// account, and the screen they were looking at while it happened:
    ///
    /// ```text
    /// · model_endpoint_retry — the model endpoint at open.bigmodel.cn did not answer:
    ///   http 429: 余额不足或无可用资源包,请充值。. Taking this round again in 1s (attempt 1 of 6).
    /// ```
    ///
    /// Six attempts, waits doubling to 32 seconds, on identical bytes, against an account that
    /// cannot pay — and the sentence was wrong twice over: the endpoint *did* answer, and what it
    /// said was actionable. The rate-limit half must survive, which is the assertion this test
    /// would be worthless without.
    #[test]
    fn a_billing_refusal_is_not_taken_again_and_a_rate_limit_still_is() {
        let said = |code: u16, body: &str| HttpError::Status {
            code,
            body: body.to_string(),
        };
        // The provider's own words, verbatim off the operator's screen.
        let credit = said(429, "http 429: 余额不足或无可用资源包,请充值。");
        assert!(
            http_retry_after(&credit, 0, MAX_HTTP_RETRIES).is_none(),
            "a billing refusal was taken again: no retry can top up an account"
        );
        // OpenAI's own billing code, which arrives as a 429 too.
        assert!(
            http_retry_after(
                &said(
                    429,
                    r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota, please check your plan and billing details"}}"#
                ),
                0,
                MAX_HTTP_RETRIES
            )
            .is_none()
        );
        // **The case the exception exists for, and it must survive.** A rate limit names
        // requests-per-minute, not money, and it is transient by construction.
        assert!(
            http_retry_after(
                &said(
                    429,
                    r#"{"error":{"type":"requests","message":"Rate limit reached for requests"}}"#
                ),
                0,
                MAX_HTTP_RETRIES
            )
            .is_some(),
            "a rate limit stopped being retried — this is the whole reason 429 is an exception"
        );
        // A status with no body at all is the old behaviour, unchanged.
        assert!(http_retry_after(&status(429), 0, MAX_HTTP_RETRIES).is_some());
    }
    /// **The budget is the caller's, and `1` means do not retry.**
    ///
    /// The ladder is right for a server that might be reloading and wrong for one
    /// that is not there, and only the caller knows which. Measured, 2026-09-20:
    /// `compact.rs` took 63.7 seconds of wall clock for 3.5 seconds of CPU — one
    /// test walking 1+2+4+8+16+32 against a port its own comment called *"a dead
    /// port, so the attempt fails fast"*. With the budget at 1 the same file takes
    /// 1.65 seconds.
    ///
    /// The premise is asserted first: the SAME error at the same attempt is still
    /// retryable under the default. Without that this passes on a build that
    /// stopped retrying altogether.
    #[test]
    fn one_attempt_means_no_retry_and_the_default_still_retries() {
        let refused = || {
            HttpError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "no",
            ))
        };
        assert!(
            http_retry_after(&refused(), 0, MAX_HTTP_RETRIES).is_some(),
            "the premise: a refused connection is retryable under the default"
        );
        assert!(
            http_retry_after(&refused(), 0, 0).is_none(),
            "zero retries means the first failure is the answer"
        );
        // And a budget in between is honoured as itself, so this is a number and
        // not a boolean wearing one. The off-by-one here is the whole reason the
        // field is named for RETRIES: `1` used to be called one attempt and still
        // retried once.
        assert!(
            http_retry_after(&refused(), 0, 1).is_some(),
            "one retry is one"
        );
        assert!(http_retry_after(&refused(), 1, 1).is_none(), "and only one");
        assert!(http_retry_after(&refused(), 2, 3).is_some());
        assert!(http_retry_after(&refused(), 3, 3).is_none());
    }

    /// Doubling from a second, and a hard stop — so an endpoint that is
    /// genuinely gone is REPORTED rather than waited on forever.
    #[test]
    fn the_wait_doubles_and_the_attempts_run_out() {
        let secs: Vec<u64> = (0..MAX_HTTP_RETRIES)
            .map(|a| {
                http_retry_after(&status(503), a, MAX_HTTP_RETRIES)
                    .expect("retryable")
                    .as_secs()
            })
            .collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 32]);
        assert_eq!(secs.iter().sum::<u64>(), 63, "about a minute in total");
        assert!(
            http_retry_after(&status(503), MAX_HTTP_RETRIES, MAX_HTTP_RETRIES).is_none(),
            "the cap is a cap"
        );
    }

    /// **A 4xx is about the request, not the weather.**
    ///
    /// The retry's premise is that *"the retry sends exactly the bytes this one
    /// did"* — which is what makes it safe, and equally what makes it pointless
    /// when the server's complaint is about those bytes. This used to retry a 400
    /// on purpose, with the note that following "every code except
    /// unauthenticated" was cheaper than keeping a list of which 5xx is
    /// transient, and that it cost about a minute.
    ///
    /// It cost that minute for real on 2026-09-19: a session switched to deepseek
    /// carried an assistant row whose `tool_calls` had no matching `tool`
    /// messages, the provider refused with a 400 saying so, and the operator
    /// watched six identical attempts with the waits doubling to 32 seconds.
    ///
    /// The list stayed small, which was the original worry: two codes.
    #[test]
    fn a_four_hundred_is_not_retried_and_a_five_hundred_still_is() {
        assert!(
            http_retry_after(&status(400), 0, MAX_HTTP_RETRIES).is_none(),
            "the bytes are the problem, so sending them again cannot help"
        );
        for code in [404, 413, 422] {
            assert!(
                http_retry_after(&status(code), 0, MAX_HTTP_RETRIES).is_none(),
                "{code}"
            );
        }
        // The two 4xx that are about timing rather than content: 408 is the server
        // saying it waited too long, 429 is it saying not yet.
        assert!(http_retry_after(&status(408), 0, MAX_HTTP_RETRIES).is_some());
        assert!(http_retry_after(&status(429), 0, MAX_HTTP_RETRIES).is_some());
        // And the one deterministic failure still waited on, stated rather than
        // hidden: llama.cpp reports the context wall as a 500, and a 5xx is not
        // something a client can tell apart from a server restarting.
        assert!(
            http_retry_after(
                &HttpError::Status {
                    code: 500,
                    body: "Context size has been exceeded".into()
                },
                0,
                MAX_HTTP_RETRIES
            )
            .is_some(),
            "the context wall, which the mid-turn check exists to prevent reaching"
        );
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
pub fn abandoned_calls(items: &[TranscriptItem]) -> Vec<(String, String)> {
    let mut asked: Vec<(String, String)> = Vec::new();
    let mut answered: Vec<String> = Vec::new();
    for item in items {
        match item {
            TranscriptItem::Assistant { tool_calls, .. } => {
                for c in tool_calls {
                    asked.push((c.id.clone(), c.name.clone()));
                }
            }
            TranscriptItem::ToolResult { call_id, .. } => answered.push(call_id.clone()),
            _ => {}
        }
    }
    // A call asked for TWICE and answered once is answered: the set difference, not a count.
    asked
        .into_iter()
        .filter(|(id, _)| !answered.iter().any(|a| a == id))
        .collect()
}
