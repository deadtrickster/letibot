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
use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::event::{TodoEntry, TodoStatus as WireTodoStatus};
use letibot_sessionlog::{LogSink, SessionEvent, ToolLogSink};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_tokencore::{Vocab, ledger::hex as hex32};
use letibot_tools::authorise::{
    AuthorisationTrail, BreakerState, DenialNotice, DenialSink, Speaker, Utterance,
};
use letibot_tools::builtins::intent::{self as intent_tools, IntentLedger, IntentSink};
use letibot_tools::builtins::todo::TodoBoard;
use letibot_tokencore::store::TodoItem;
use letibot_tools::exec::monitor::Monitors;
use letibot_tools::{
    AdjudicatedGate, Adjudicator, Gate, GateCall, HostBackend, NoBoundary, Registry, Role,
    ToolRuntime, Tool, roles,
};
use letibot_transcript::{SystemOrigin, ToolCall, TranscriptItem, UserPart};
use letibot_turn::{
    CompactionOutcome, Endpoint, EventSink, Session, SteeringMessage, SteeringSource, TurnEngine,
    TurnEvent, TurnFailure, TurnMetrics, TurnOk, OverrunPlan, plan_fold, plan_overrun, run_compaction,
    summarise_first_half, summarise_overrun,
};

use crate::config::{AdjudicatorChoice, Config, GateWiring, Seat, SpillPolicy, SpillStorage};
use crate::dialect::Wiring;
use crate::jobwatch::{JobWatchSink, JobWatchers};
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
}

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
        let skills = std::sync::Arc::new(letibot_tools::builtins::skill::SkillRegistry::load_default());
        let lsp = std::sync::Arc::new(letibot_tools::builtins::lsp::LspConfig::default());
        let tasks = std::sync::Arc::new(crate::tasks::TaskJournal::new(
            crate::tasks::default_state_path(),
            lsp.clone(),
            skills.clone(),
        ));
        Ok(Parts {
            vocab: std::sync::Arc::new(vocab),
            wiring: std::sync::Arc::new(cfg.dialect.wiring(cfg.effort.as_deref())),
            mode_store: std::sync::Arc::new(std::sync::RwLock::new(crate::modes::ModeStore::open())),
            tasks,
            lsp,
            skills,
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
    LoopBound { rounds: usize },
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
    NoProgress { evidence: String },
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
    /// Every `User` row comes back as [`Speaker::Operator`] and that is the honest
    /// reading available here: the store does not record which of them the harness
    /// injected. It is the **conservative** direction for the denominator and the
    /// permissive one for authorisation, so it is called out in the resume notes
    /// rather than left as a property nobody knows about.
    fn seed(&self, items: &[TranscriptItem]) {
        let mut g = self.lock();
        g.items = items.len();
        g.said.clear();
        for (i, item) in items.iter().enumerate() {
            if let TranscriptItem::User { parts } = item {
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
                    speaker: Speaker::Operator,
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

/// Steering from the head socket (§5.8), from the intent check, and from a fired
/// monitor.
///
/// A `Prompt` that arrives while the model is generating is a correction, and it
/// goes in at the next step boundary. An `Interrupt` is the urgent form: it stops
/// generation at the next token and the partial output is kept.
///
/// The hub's queue is drained here *only while a turn is running*; between turns
/// the daemon's own worker owns it. One drainer at a time, by construction of who
/// is executing.
///
/// # Three sources, one queue, and the speaker is recorded as they are drained
///
/// The step boundary is the only place any of the three can be injected, so they
/// share this source rather than each growing a path into the engine. What they do
/// **not** share is provenance: a head's prompt is the operator, and the intent
/// check and a monitor firing are this harness talking to itself. That distinction
/// is recorded here, at the moment the message is handed over, because after the
/// engine appends it they are four identical `User` items. See [`TrailMirror`].
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
}

impl HubSteering {
    pub fn new(hub: Arc<Hub>) -> Self {
        HubSteering {
            hub,
            trail: None,
            injected: Arc::new(Mutex::new(VecDeque::new())),
            monitors: None,
            monitor_cursor: Arc::new(AtomicUsize::new(0)),
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

impl SteeringSource for HubSteering {
    fn try_next(&mut self) -> Option<SteeringMessage> {
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
                CommandKind::Interrupt { reason } => Some(SteeringMessage::urgent(reason)),
                // Nothing else here: the filter above only hands over prompts and
                // interrupts, and anything else stays queued for the worker.
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
}

/// One session: the engine, the transcript, the tools, the log and the store.
pub struct Harness<'a> {
    cfg: Config,
    /// The daemon's session registry, kept so the settings this session runs
    /// under can be republished on every runtime change (`/mode`, `/supervise`).
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
    pub tail_items: usize,
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

fn tool_names(tools_json: &[String]) -> std::collections::BTreeSet<String> {
    tools_json
        .iter()
        .filter_map(|j| {
            let v: serde_json::Value = serde_json::from_str(j).ok()?;
            let n = v.pointer("/function/name").or_else(|| v.get("name"))?.as_str()?;
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
        let corpus_sink: Option<std::sync::Arc<dyn letibot_tools::CorpusSink>> = opened_corpus
            .map(|c| c as std::sync::Arc<dyn letibot_tools::CorpusSink>);
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
            if store.is_set(&cfg.workspace) {
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
                HostBackend::new(&cfg.workspace)
                    .map_err(|e| HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace)))?,
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
                HostBackend::writable(&cfg.workspace)
                    .map_err(|e| HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace)))?,
                false,
            )
        } else {
            (
                HostBackend::new(&cfg.workspace)
                    .map_err(|e| HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace)))?,
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
        let scratch = std::env::temp_dir().join(format!("letibot-scratch-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&scratch);
        let backend = backend.with_scratch_dir(&scratch);
        // One boxed backend from here on, whichever substrate: the host one built
        // above, or a firecode VM booted on a copy of the workspace.
        let (backend, backend_described, backend_writable, monitors): (
            Box<dyn letibot_tools::backend::ExecBackend>,
            String,
            bool,
            Option<Arc<Monitors>>,
        ) = if in_vm {
            let mut spec = letibot_tools::firecode::FirecodeSpec::new(&cfg.workspace, &cfg.session_id);
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
                    ("LETIBOT_SOCKET".to_string(), cfg.socket.display().to_string()),
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
                        return Err(HarnessError::Setup(format!(
                            "--web-search brave: {why}"
                        )));
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
                slots: Default::default(),
            });
        // Cloned before `with_session_tools` takes it: `digest` folds its findings
        // through the same subagent runner `task` uses, so the two must be the
        // same object — a digest running on a second runner would be a subagent
        // tree the operator's `task_result` listing does not show.
        let digest_runner = task_runner.clone();
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
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::jobs::JobKill)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::monitor::Monitor)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::pkill::Pkill)))
            .and_then(|_| registry.register(Box::new(letibot_tools::builtins::ps::Ps)))
            .map_err(|e| HarnessError::Setup(format!("registering the exec tools: {e}")))?;
        let mut registry = letibot_tools::external_tools(registry, &external)
            .map_err(|e| HarnessError::Setup(format!("registering the outside-world tools: {e}")))?;
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
                    registry.register(Box::new(
                        letibot_tools::builtins::digest::DigestTool::new(src, digest),
                    ))
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
            adjudicator, gated,
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
                    Some(_) => Some(model_adjudicator(&cfg, "`--oracle`", Some(hub.clone()))?.into()),
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
                    .with_permission(downgraded_ruleset(&cfg.permission, &cfg.downgrade, &schemas))
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
                    Some(a) => g
                        .with_advisor(a)
                        .start_supervised(cfg.supervise || cfg.mode.decider == letibot_tools::mode::Decider::Model),
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
        let job_watch = backend.processes_arc().map(|h| JobWatchers::new(&h, &hub));
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
        let runtime = ToolRuntime::new(registry, backend)
            .with_spiller(spiller)
            .with_gate(gate);

        let engine = TurnEngine::new(
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
                            .map_err(|e| HarnessError::Store(format!("re-rendered row {i}: {e}")))?;
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
                    (new_id, rebuilt, rows, Some(report), prefix.clone(), new_prefix_id)
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
                            ". `/reseat` rebuilds the prompt from what is seated now,                              forking the conversation onto it the way a compaction does;                              a new session gets the seated list from the start.",
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
        // **Fill the harness view's slot, now that there is something to disclose.**
        // The tool was registered before the gate and the backend existed — it had
        // to be, to be in the prompt — and this is the first point where the
        // disclosures it reports can be computed.
        *disclosure_slot.lock().unwrap_or_else(|e| e.into_inner()) = cfg
            .disclosures(&wiring)
            .into_iter()
            .map(|d| (d.subject, d.state, d.detail))
            .collect();
        let h = Harness {
            session_registry: session_registry.clone(),
            mode_source,
            wiring,
            cfg,
            engine,
            session,
            runtime,
            hub,
            store,
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
            trail,
            todos: todo_board,
            todos_version: 0,
            intent,
            injected,
            monitors,
            monitor_cursor,
            tool_sink,
            job_watch,
            provider,
            // Filled on the first switch away from local, never at open: a session
            // that started on a provider has no local window to go back to, and
            // `None` here says exactly that.
            local_window: None,
        };
        h.publish_settings();
        if h.resumed.is_some() {
            h.republish();
        }
        Ok(h)
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
    fn republish(&self) {
        for (i, row) in self.session.ledger.rows().iter().enumerate() {
            let Some(item) = self.session.items.get(i) else {
                continue;
            };
            self.hub
                .publish(letibot_sessionlog::SessionEvent::TranscriptAppended {
                    item_id: row.item_id.clone(),
                    kind: letibot_tokencore::store::item_kind(item).to_string(),
                    ledger_head: hex32(&row.h_k),
                });
            self.hub.record_item(&row.item_id, item.clone());
        }
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
        if let Some((have, seats)) = &self.supplies {
            mode.check(have, *seats)?;
        }
        let dropped = self.runtime.gate.set_mode(mode)?;
        let mut said = format!("this session is at `{}` from the next call", mode.name);
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
            self.cfg.settings(&self.mode_source, self.runtime.gate.supervising()),
        );
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
        self.session.ledger.rows().get(i).map(|r| r.tok_len).unwrap_or(0)
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
            parts: vec![UserPart::Text { text: text.into() }],
        })
    }

    /// This session's monitors, or `None` on a backend that cannot start processes.
    ///
    /// The daemon needs them to arm the wake; nothing else does.
    pub fn monitors(&self) -> Option<&Arc<Monitors>> {
        self.monitors.as_ref()
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
        match choice {
            None => {
                self.provider = None;
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
                self.provider = Some(p);
                self.cfg.provider = Some(pc);
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
    pub fn job_lines(&self) -> Vec<String> {
        let Some(host) = self.runtime.backend.processes() else {
            return vec!["this session has no process host, so it has no jobs".into()];
        };
        let jobs = host.jobs();
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
                j.command.chars().take(60).collect::<String>()
            ));
        }
        out.push(String::new());
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
            return Err(format!("no job `{job}` here; `/job` with no argument lists them"));
        };
        let slice = host.output(&jid, offset, limit).map_err(|e| e.to_string())?;

        // A job that has written nothing is not an empty answer about its output:
        // whether it is still running decides what the silence means. The same
        // distinction `job_output` draws, in the same words.
        if slice.produced == 0 {
            return Ok(vec![if view.state.is_running() {
                format!("`{job}` is still running and has written nothing yet.")
            } else {
                format!("`{job}` {} and wrote nothing at all.", view.state.word())
            }]);
        }

        let mut out: Vec<String> = slice.text().lines().map(str::to_string).collect();
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

        let mut out = vec![format!(
            "{} tool(s) seated in this session.",
            schemas.len()
        )];
        out.push(String::new());
        let mut by_name = schemas;
        by_name.sort_by(|a, b| a.name.cmp(&b.name));
        for s in &by_name {
            // A tool the prompt has never heard of is marked where the eye already
            // is, rather than only in a footnote below the list.
            let mark = if announced.contains(&s.name) { "  " } else { "! " };
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
                 `/reseat` does the same without waiting for the context to fill."
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
            None => format!("local — {} at {}", self.cfg.model, self.cfg.endpoint.authority()),
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

    /// **Something fired while nothing was running.** T24's wake, from the worker.
    ///
    /// The daemon calls this when [`letibot_sessionlog::registry::Work::Woken`]
    /// names this session. Every monitor that settled since the cursor becomes one
    /// user item and the loop runs — so a condition that happened between turns is
    /// acted on, rather than sitting in `job_list` until the model happens to ask.
    ///
    /// `Ok(None)` means nothing had settled after all: a wake that raced a mid-turn
    /// pickup by [`HubSteering`], which shares the cursor. That is a real outcome
    /// and not a failure, and returning `None` rather than running an empty turn is
    /// what stops a spurious wake from costing a generation.
    pub fn wake(&mut self) -> Result<Option<Reply>, HarnessError> {
        let Some(monitors) = self.monitors.clone() else {
            return Ok(None);
        };
        let since = self.monitor_cursor.load(Ordering::SeqCst);
        let settled = monitors.settled_count();
        if settled <= since {
            return Ok(None);
        }
        let fired: Vec<_> = monitors.firings().into_iter().skip(since).collect();
        self.monitor_cursor.store(settled, Ordering::SeqCst);
        if fired.is_empty() {
            return Ok(None);
        }
        let text = monitor_notice(&fired);
        // The harness talking to itself, not the operator. A firing must never be
        // able to authorise the action it reports on.
        self.trail.begin_turn();
        self.trail
            .say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            parts: vec![UserPart::Text { text }],
        })
        .map(Some)
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
            parts: vec![UserPart::Text { text: text.into() }],
        })
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
            let outcome = run_compaction(&mut self.engine, &mut self.session, &mut sink)
                .map_err(HarnessError::Turn)?;
            if outcome.tool_calls > 0 {
                return Err(HarnessError::Setup(format!(
                    "the summary turn proposed {} tool call(s); a summary is a record, not                      an action, so nothing was re-seated.",
                    outcome.tool_calls
                )));
            }
            let fork = self.fork_to_summary(&outcome, Some(&next), Some(&next_id), &[])?;
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
            .map(|i| self.session.ledger.item_tokens(i).map(|t| t.len() as u64).unwrap_or(0))
            .collect();
        let prefix_tokens = self.session.ledger.prefix_len() as u64;
        let window = self.cfg.context_window.unwrap_or(u64::MAX);

        match plan_overrun(&per_item, prefix_tokens, window) {
            // The ordinary path, and the one that runs almost always.
            OverrunPlan::NotOverrun => {
                let outcome = run_compaction(&mut self.engine, &mut self.session, &mut sink)
                    .map_err(HarnessError::Turn)?;
                if outcome.tool_calls > 0 {
                    return Err(HarnessError::Setup(format!(
                        "the summary turn proposed {} tool call(s); a summary is a record, \
                         not an action, so nothing was compacted. The transcript is \
                         unchanged apart from the instruction and the turn themselves — \
                         try again.",
                        outcome.tool_calls
                    )));
                }
                let fork = self.fork_to_summary(
                    &outcome,
                    reseat.as_ref().map(|(p, _)| p),
                    reseat.as_ref().map(|(_, id)| id.as_str()),
                    &[],
                )?;
                let (gained, lost) = self.adopt_reseat(reseat);
                Ok(CompactReport { fork, summary_turn: outcome, gained, lost })
            }

            OverrunPlan::Hopeless { prefix_tokens, window } => Err(HarnessError::Setup(format!(
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
                let (harvest, tail) = if self.provider.is_some() {
                    match plan_fold(&per_item, prefix_tokens, window) {
                        Some(split) => {
                            self.hub.publish(SessionEvent::Warning {
                                code: "auto_compact".into(),
                                detail: format!(
                                    "over budget: summarising the first {split} item(s) and \
                                     continuing on the summary plus the rest, verbatim"
                                ),
                            });
                            let h = summarise_first_half(
                                &mut self.engine, &prefix, &scratch, &items, split, &mut sink,
                            )
                            .map_err(HarnessError::Turn)?;
                            (h, items[split..].to_vec())
                        }
                        // No workable fold: fall through to the two-half plan,
                        // which asks less of the split.
                        None => {
                            let h = summarise_overrun(
                                &mut self.engine, &prefix, &scratch, &items, &plan, &mut sink,
                            )
                            .map_err(HarnessError::Turn)?;
                            (h, Vec::new())
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
                    });
                    let h = summarise_overrun(
                        &mut self.engine, &prefix, &scratch, &items, &plan, &mut sink,
                    )
                    .map_err(HarnessError::Turn)?;
                    (h, Vec::new())
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
                let fork = self.fork_to_summary(
                    &outcome,
                    reseat.as_ref().map(|(p, _)| p),
                    reseat.as_ref().map(|(_, id)| id.as_str()),
                    &tail,
                )?;
                let (gained, lost) = self.adopt_reseat(reseat);
                Ok(CompactReport { fork, summary_turn: outcome, gained, lost })
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
        tail: &[TranscriptItem],
    ) -> Result<ForkReport, HarnessError> {
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
        let note: TranscriptItem = TranscriptItem::System {
            text: format!(
                "This conversation was compacted: everything said before this point is \
                 replaced by the summary below, which was written over the full history \
                 of transcript {old_id} and proposed no tool calls.{cut}\n\n{}",
                outcome.summary
            ),
            origin: SystemOrigin::Update,
        };
        // **The recent past is carried over verbatim, not described.**
        //
        // Compaction used to keep none of it: the new base was the summary and
        // nothing else, so a session came back from a compaction unable to see the
        // turn it was in the middle of -- that turn was now a sentence about a
        // turn. `tail` is the newest items, under their own budget, appended after
        // the summary so the order of the conversation is preserved: everything
        // old as prose, then the last stretch as itself.
        let mut body: Vec<TranscriptItem> = Vec::with_capacity(1 + tail.len());
        body.push(note);
        body.extend_from_slice(tail);

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
        Ok(ForkReport {
            transcript_id: new_id,
            parent_id: old_id,
            forked_at,
            was_tokens,
            base_tokens: self.session.ledger.len(),
                    truncated: outcome.truncated,
            tail_items: tail.len(),
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
            TranscriptItem::User { parts } => parts.iter().find_map(|p| match p {
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

        for round in 0..self.cfg.max_tool_rounds {
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
                && let Some(window) = self.cfg.context_window
                && self.cfg.auto_compact
            {
                let resident = self.session.ledger.len() as u64;
                if resident + self.cfg.headroom() >= window {
                    self.hub.publish(SessionEvent::Warning {
                        code: "context_wall".into(),
                        detail: format!(
                            "stopping this turn after {round} round(s): {resident} of                              {window} tokens are resident and the next round needs                              {} free. Everything so far is committed, and the session compacts before the next turn — this is the wall, not a failure of the work.",
                            self.cfg.headroom()
                        ),
                    });
                    eprintln!(
                        "  context wall: stopped after {round} round(s) at {resident} of {window} tokens"
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
                let attempted = match &self.provider {
                    None => self
                        .engine
                        .run_turn_steered(&mut self.session, &mut sink, &mut steering),
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
                let Err(TurnFailure::Http(e)) = attempted else {
                    attempt = 0;
                    break attempted;
                };
                let Some(wait) = http_retry_after(&e, attempt) else {
                    break Err(TurnFailure::Http(e));
                };
                attempt += 1;
                self.hub.publish(SessionEvent::Warning {
                    code: "model_endpoint_retry".into(),
                    detail: format!(
                        "the model server at {} did not answer: {e}. Taking this round \
                         again in {:.0}s (attempt {attempt} of {MAX_HTTP_ATTEMPTS}). \
                         Nothing was recorded, so the retry sends exactly the bytes this \
                         one did.",
                        self.cfg.endpoint.authority(),
                        wait.as_secs_f64(),
                    ),
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
            let ok: TurnOk =
                match outcome
                {
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
            // made an injection ordinary rather than rare. Append order is the model's
            // items first, then the steering, and `reconcile` zips positionally, so the
            // order here is load-bearing.
            let mut appended = ok.items.clone();
            appended.extend(ok.steering_applied.iter().cloned());
            self.reconcile(&mut sink, &appended);
            self.persist()?;
            // The last round's prompt size is the session's context size, and the
            // session row is the only place it survives a restart: a head that
            // attaches to a rebuilt view has no turn state to read it from.
            // Per round, the same way the heads' own `TurnFinished` updates theirs,
            // so the row and the screen agree at every point a round has landed.
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
                }
                results.push(item);
            }
            let round_verdict = progress.end_round();
            let mut sink = CapturingSink::new(self.hub.clone());
            self.session
                .append_items(&self.engine, &results, &mut sink)?;
            self.reconcile(&mut sink, &results);
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
        Err(HarnessError::LoopBound {
            rounds: self.cfg.max_tool_rounds,
        })
    }

    /// This session's steering source: the head's queue, the intent findings, and
    /// a monitor that fires mid-turn.
    ///
    /// Rebuilt per round because it borrows nothing and holds only clones; what it
    /// must **share** is the trail, the injected queue and the monitor cursor, so a
    /// firing picked up in one round is not delivered again in the next.
    fn steering(&self) -> HubSteering {
        HubSteering {
            hub: self.hub.clone(),
            trail: Some(self.trail.clone()),
            injected: self.injected.clone(),
            monitors: self.monitors.clone(),
            monitor_cursor: self.monitor_cursor.clone(),
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
        let ids = sink.take_ids();
        if ids.len() != items.len() {
            self.hub
                .publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "record_item_pairing".into(),
                    detail: format!(
                        "{} TranscriptAppended events for {} items; the head will show \
                         empty rows. This is a daemon bug, not a transport one.",
                        ids.len(),
                        items.len()
                    ),
                });
        }
        for (id, item) in ids.iter().zip(items) {
            self.hub.record_item(id, item.clone());
        }
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
    /// three words.
    pub(crate) fn todo_entry(t: TodoItem) -> TodoEntry {
        TodoEntry {
            content: t.content,
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
            )
            .map_err(|e| HarnessError::Store(format!("context: {e}")))
    }
}


/// How many times a round is re-attempted when the model endpoint fails.
///
/// Six, doubling from a second: 1, 2, 4, 8, 16, 32 — about a minute of waiting
/// before the turn fails for real. That is long enough to sit out the thing this
/// exists for (llama.cpp reloading a six-shard GGUF after `--sleep-idle-seconds`,
/// which answers `503 Loading model` for as long as it takes) and short enough
/// that an endpoint which is genuinely gone is reported rather than waited on.
const MAX_HTTP_ATTEMPTS: u32 = 6;

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
fn http_retry_after(e: &letibot_turn::HttpError, attempt: u32) -> Option<std::time::Duration> {
    if attempt >= MAX_HTTP_ATTEMPTS {
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
        letibot_turn::HttpError::Status { code, .. } => {
            matches!(code, 408 | 429) || *code >= 500
        }
    };
    worth_it.then(|| std::time::Duration::from_secs(1u64 << attempt))
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
    const MAX: usize = 48;
    if out.chars().count() > MAX {
        out = out.chars().take(MAX - 1).collect::<String>();
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

/// A subagent's adjudicator: it can never ask the operator, so every `ask` fails
/// closed. `describe` deliberately does not begin with `none` — that prefix is the
/// harness's own signal for "nobody reachable, refuse to open a gated session" —
/// because a subagent *does* have a decider: the permission ruleset it inherited.
/// The ruleset's `allow`/`deny` are honoured by the gate before this is reached;
/// what reaches it is only what the ruleset left as `ask`, and that is denied.
struct SubagentAdjudicator;

impl letibot_tools::Adjudicator for SubagentAdjudicator {
    fn decide(
        &self,
        req: &letibot_tools::AdjudicationRequest,
    ) -> letibot_tools::AdjudicationDecision {
        letibot_tools::AdjudicationDecision::unavailable(
            req,
            "subagent",
            "a subagent has no operator to ask; the inherited permission rules decide, \
             and an ask is denied (fail closed)",
        )
    }

    fn describe(&self) -> String {
        "subagent — inherited permission rules decide; asks are denied".into()
    }
}

/// The real `task` runner: spawns a **persistent subagent session** and runs it to
/// completion, returning the subagent's final answer to the parent.
///
/// A subagent is a full session — its own hub (an operator can attach and message
/// it), its own store row (it survives the daemon and can be resumed), and its own
/// harness — not an ephemeral nested turn. It inherits the parent's permission
/// ruleset and mode, so its calls are governed by the same allow/deny/ask rules,
/// but it can never ask the operator: an `ask` fails closed on
/// [`SubagentAdjudicator`].
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
struct TaskSlot {
    state: std::sync::Mutex<letibot_tools::builtins::task::TaskStatus>,
    settled: std::sync::Condvar,
}

impl TaskSlot {
    fn new() -> TaskSlot {
        TaskSlot {
            state: std::sync::Mutex::new(letibot_tools::builtins::task::TaskStatus::Running {
                note: None,
            }),
            settled: std::sync::Condvar::new(),
        }
    }

    /// The child's own last word about what it is doing. Kept only while it is
    /// running: a note on a settled subagent would overwrite its answer.
    fn note(&self, text: &str) {
        let mut g = self.state.lock().expect("task slot");
        if let letibot_tools::builtins::task::TaskStatus::Running { note } = &mut *g {
            *note = Some(text.to_string());
        }
    }

    fn settle(&self, status: letibot_tools::builtins::task::TaskStatus) {
        *self.state.lock().expect("task slot") = status;
        self.settled.notify_all();
    }
}

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
        // The id is minted here rather than inside the body, because it is what
        // this call returns and the body has not run yet.
        let sub_id = format!(
            "{}-sub-{}",
            self.base.session_id,
            letibot_sessionlog::registry::now_ms()
        );
        // The seat is checked NOW: a role this build does not know is a fact
        // about the call, and answering it from a thread would report "started"
        // for something that never could.
        let role = spec.role.as_str();
        if !(role.is_empty() || role == "coder") {
            Seat::parse(role)?;
        }
        let slot = Arc::new(TaskSlot::new());
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
        };
        let id = sub_id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("subagent-{}", letibot_sessionlog::registry::short_id(&sub_id)))
            .spawn(move || {
                let slot2 = slot.clone();
                let status = match me.run_to_completion(&id, &prompt, &spec, &mut |n| slot2.note(n))
                {
                    Ok(answer) => letibot_tools::builtins::task::TaskStatus::Done { answer },
                    Err(why) => letibot_tools::builtins::task::TaskStatus::Failed { why },
                };
                slot.settle(status);
            });
        if let Err(e) = spawned {
            // Nothing is running; say so rather than handing back a handle for a
            // child that was never started.
            self.slots.lock().expect("task slots").retain(|(h, _)| h != &sub_id);
            return Err(format!("the subagent thread could not be started: {e}"));
        }
        Ok(sub_id)
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
}

impl HarnessTaskRunner {
    fn run_to_completion(
        &self,
        sub_id: &str,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
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
        // A subagent is a real session: mint its id and create its hub. The id is a
        // nanosecond timestamp suffix rather than a counter, so a subagent minted
        // after a daemon restart cannot collide with a persisted one (a counter
        // resets, and a collision would resume the old subagent instead of spawning a
        // new one). The title is the subtask's first line, so a picker row says what
        // the subagent was for.
        let spawned = std::time::Instant::now();
        let sub_id = sub_id.to_string();
        let title = derive_title(prompt);
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
        let publish = |state: &str, prompt: &str| {
            if let Some(hub) = self.registry.get(&parent) {
                hub.publish(SessionEvent::Subagent {
                    subagent_id: sub_id.clone(),
                    state: state.to_string(),
                    prompt: prompt.to_string(),
                    role: seat.as_str().to_string(),
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
        publish("opening", &title);
        progress(&match placement {
            Placement::Firecode => format!(
                "opening subagent {} in a firecode VM: copying the workspace, booting",
                letibot_sessionlog::registry::short_id(&sub_id)
            ),
            _ => format!("opening subagent {}", letibot_sessionlog::registry::short_id(&sub_id)),
        });
        // Every early return from here on records the failure rather than leaving a
        // "running" row forever.
        let fail = |why: String| {
            self.tasks.record(crate::tasks::TaskEntry {
                name: sub_id.clone(),
                role: seat.as_str().to_string(),
                state: "failed".into(),
                tokens: 0,
                elapsed: spawned.elapsed().as_secs_f64(),
                prompt: why.clone(),
                parent: parent.clone(),
            });
            publish("failed", &why);
            why
        };

        let wiring = letibot_sessionlog::registry::SessionWiring {
            model: self.base.model.clone(),
            dialect: self.base.dialect.name().to_string(),
            endpoint: self.base.endpoint.authority(),
            workspace: self.base.workspace.display().to_string(),
        };
        // Built, not registered: nothing can switch into it, and the worker is not
        // told to open it, until `open_with_registry` below has succeeded.
        let sub_hub = self.registry.new_hub(sub_id.clone());

        let sub_cfg = Config {
            session_id: sub_id.clone(),
            title: title.clone(),
            seat,
            parent_session_id: Some(parent.clone()),
            downgrade,
            placement,
            ..self.base.clone()
        };

        // Reassemble the shared parts so the sub harness can borrow them for the
        // duration of this call. Cheap: the vocabs and the wiring are already `Arc`.
        let parts = Parts {
            vocab: self.vocab.clone(),
            wiring: self.wiring.clone(),
            mode_store: self.mode_store.clone(),
            tasks: self.tasks.clone(),
            lsp: self.lsp.clone(),
            skills: self.skills.clone(),
        };

        let mut sub = Harness::open_with_registry(
            &parts,
            sub_cfg,
            sub_hub.clone(),
            Some(Box::new(SubagentAdjudicator)),
            None,
            self.registry.clone(),
        )
        .map_err(|e| fail(e.to_string()))?;
        // Open. Now it is a session a head can switch into, and now it is running.
        self.registry
            .adopt(sub_hub, title.clone(), wiring, Some(parent.clone()))
            .map_err(|e| fail(e.to_string()))?;
        self.tasks.record(crate::tasks::TaskEntry {
            name: sub_id.clone(),
            role: seat.as_str().to_string(),
            state: "running".into(),
            tokens: 0,
            elapsed: spawned.elapsed().as_secs_f64(),
            prompt: title.clone(),
            parent: parent.clone(),
        });
        publish("running", &title);
        progress(&format!(
            "subagent {} open after {:.1}s — running; ctrl-g lists it, enter attaches",
            letibot_sessionlog::registry::short_id(&sub_id),
            spawned.elapsed().as_secs_f64()
        ));

        let reply = sub.submit(prompt).map_err(|e| fail(e.to_string()))?;
        // The child is done: release its substrate now, not when the harness is
        // dropped, so the parent is told where the work went in the same reply.
        let landed = sub.close_backend();
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
        publish("done", &first_line);
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
        for k in ["temperature", "top_p", "seed", "max_tokens", "presence_penalty", "frequency_penalty"] {
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
        (Access::Network, &["flowy", "web_search", "web_fetch", "forge", "mcp"]),
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
                        "bash" | "job_list" | "job_output" | "job_wait" | "job_kill" | "monitor" | "pkill" | "ps"
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
pub fn surroundings_for(cfg: &Config) -> letibot_tools::Surroundings {
    let env = letibot_tools::Surroundings::from_env(cfg.workspace.display().to_string())
        .with_known_hosts();
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
mod tests {
    use super::*;
    use letibot_tools::authorise::TrailProvenance;

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User {
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
        assert!(denied.contains(&"edit") && denied.contains(&"write") && denied.contains(&"odd_writer"), "{denied:?}");
        assert!(!denied.contains(&"read") && !denied.contains(&"bash") && !denied.contains(&"flowy"), "{denied:?}");
        // Last rule wins: the parent's allow for `edit` is overridden.
        let r = letibot_tools::permission::evaluate("edit", "anything", &[&rules]);
        assert_eq!(r.action, Action::Deny);
        // No downgrade, no change.
        assert_eq!(downgraded_ruleset(&parent, &Downgrade::none(), &seated), parent);
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
        // The whole opencode union plus the room fits the ceiling exactly; a tool
        // added to leticode after this has to take a seat from something.
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
        m.say(Speaker::Operator, "ssh to lubuntu2 and check the build", None);
        m.say(Speaker::Agent, "[intent check] you said you would ssh", None);

        let t = m.trail();
        let words: Vec<&str> = t.operator_words().iter().map(|u| u.text.as_str()).collect();
        assert_eq!(words, vec!["ssh to lubuntu2 and check the build"]);
        assert_eq!(t.utterances.len(), 2, "both are carried, with their speakers");
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
        assert!(t.was_collected(), "somebody looked; this is not `NotCollected`");
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

    /// A rebuilt transcript seeds the trail, and the reading is the permissive one.
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
    let guard_model = cfg.oracle_model.clone().unwrap_or_else(|| cfg.model.clone());
    let mut oracle = crate::oracle::HttpOracle::new(ep, guard_model, cfg.oracle_budget)
        .with_question(cfg.oracle_question);
    if let Some(scope) = &cfg.oracle_scope {
        oracle = oracle.with_scope(scope.clone());
    }
    // Layer A, re-derived per request from the command as the program will receive
    // it. Not copied from the request's own `baseline` string: that is prose for a
    // human, and the adjudicator needs the classification.
    let surroundings = letibot_tools::intent::Surroundings::default();
    Ok(Box::new(letibot_tools::ModelAdjudicator::new(
        Box::new(oracle),
        move |req: &letibot_tools::AdjudicationRequest| {
            let cmd = req
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            letibot_tools::intent::Baseline::of_command(cmd, &surroundings)
        },
    )
    // **The wait, announced before it starts.** Consulting the guard costs
    // seconds on this box, and a turn that pauses with nothing on the screen
    // reads as a hang — the operator went to `htop` to find out whether
    // anything was wrong. Nothing was; the guard was deciding, and nobody
    // said so. `ToolProgress` is the one the head already renders as the
    // latest note on the running call.
    .with_notice(move |req: &letibot_tools::AdjudicationRequest, note: &str| {
        if let Some(h) = &hub {
            h.publish(SessionEvent::ToolProgress {
                turn_id: req.turn_id.clone(),
                call_id: req.call_id.clone(),
                note: note.to_string(),
            });
        }
    })))
}

#[cfg(test)]
mod endpoint_retry {
    //! **When the model server is not answering, take the round again.** The
    //! operator, 2026-09-17: *"implement exponential backoff and auto turn
    //! restart for when model http endpoint doesnt answer or answers with error
    //! codes except unauthenticated"*.
    use super::{MAX_HTTP_ATTEMPTS, http_retry_after};
    use letibot_turn::HttpError;

    fn status(code: u16) -> HttpError {
        HttpError::Status { code, body: String::new() }
    }

    /// The exception is the whole point: a credential the server rejected is
    /// rejected identically every time, so a minute of backoff buys nothing and
    /// buries the real cause under six notices.
    #[test]
    fn a_credential_the_server_refuses_is_not_retried() {
        assert!(http_retry_after(&status(401), 0).is_none());
        assert!(http_retry_after(&status(403), 0).is_none());
        // And not on a later attempt either — it is the code, not the streak.
        assert!(http_retry_after(&status(401), 3).is_none());
    }

    /// The case this exists for: llama.cpp reloading a six-shard GGUF after
    /// `--sleep-idle-seconds`, answering `503 Loading model` until it is up.
    /// Seen repeatedly on this box on 2026-09-17.
    #[test]
    fn a_server_that_is_coming_back_up_is_waited_out() {
        for code in [500, 502, 503, 504, 429] {
            assert!(
                http_retry_after(&status(code), 0).is_some(),
                "{code} should be waited out"
            );
        }
        // No answer at all, and a body that did not parse — both likeliest to be
        // a server going down mid-answer.
        assert!(
            http_retry_after(
                &HttpError::Io(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "no")),
                0
            )
            .is_some()
        );
        assert!(http_retry_after(&HttpError::Malformed("truncated".into()), 0).is_some());
    }

    /// Doubling from a second, and a hard stop — so an endpoint that is
    /// genuinely gone is REPORTED rather than waited on forever.
    #[test]
    fn the_wait_doubles_and_the_attempts_run_out() {
        let secs: Vec<u64> = (0..MAX_HTTP_ATTEMPTS)
            .map(|a| http_retry_after(&status(503), a).expect("retryable").as_secs())
            .collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 32]);
        assert_eq!(secs.iter().sum::<u64>(), 63, "about a minute in total");
        assert!(
            http_retry_after(&status(503), MAX_HTTP_ATTEMPTS).is_none(),
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
            http_retry_after(&status(400), 0).is_none(),
            "the bytes are the problem, so sending them again cannot help"
        );
        for code in [404, 413, 422] {
            assert!(http_retry_after(&status(code), 0).is_none(), "{code}");
        }
        // The two 4xx that are about timing rather than content: 408 is the server
        // saying it waited too long, 429 is it saying not yet.
        assert!(http_retry_after(&status(408), 0).is_some());
        assert!(http_retry_after(&status(429), 0).is_some());
        // And the one deterministic failure still waited on, stated rather than
        // hidden: llama.cpp reports the context wall as a 500, and a 5xx is not
        // something a client can tell apart from a server restarting.
        assert!(
            http_retry_after(
                &HttpError::Status { code: 500, body: "Context size has been exceeded".into() },
                0
            )
            .is_some(),
            "the context wall, which the mid-turn check exists to prevent reaching"
        );
    }
}

