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

/// How many of the operator's own utterances the trail carries. The bound is small because the
/// sentence that authorised an action is nearly always the last one or two, and the denominator
/// travels separately, so a short trail cannot read as a complete one.
const TRAIL_UTTERANCES: usize = 12;

/// How far back the walk goes, and how many utterances the trail holds: the only reader takes the
/// last `TRAIL_SCAN`, so anything older was held for the life of the session and never read.
pub(crate) const TRAIL_SCAN: usize = 60;

/// The longest an utterance travels verbatim, matching `Utterance::operator`'s own rule: a
/// truncated authorisation that reads as complete is worse than a missing one, so a clipped one
/// says it was clipped. It is also how much is stored.
pub(crate) const UTTERANCE_CHARS: usize = 600;

/// Everything the engine borrows for the life of the daemon. Held apart from [`Harness`] because
/// `TurnEngine<'a>` borrows the vocabulary and the renderer, and a struct owning both the borrow and
/// the borrowed would be self-referential. Every field is an `Arc`, so a clone is a few refcounts.
#[derive(Clone)]
pub struct Parts {
    pub vocab: std::sync::Arc<Vocab>,
    pub wiring: std::sync::Arc<Wiring>,
    /// Per-project mode store, loaded once at daemon start: a session's point is its project's row
    /// (longest ancestor wins), not the daemon's `--mode` flag. Locked because `/mode` writes it
    /// while session opens read it.
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
    /// Every live todo board this daemon holds, by session id: the map a parent's `todo_write` with
    /// a `target` resolves its child through. It rides [`Parts`] because it belongs to the daemon,
    /// and the store path rides along because the resolver's flush runs on another thread.
    pub child_boards: std::sync::Arc<crate::child_todos::ChildBoards>,
    /// The watcher set of the subagent TREE this session belongs to, when it is a child (R58).
    /// `None` for a root, whose set is its own; a child carries its parent's, so the tree shares the
    /// watching bookkeeping, the level above and the ROOT a permission card can be drawn by.
    pub tree_watch: Option<std::sync::Arc<crate::jobwatch::JobWatchers>>,
    /// The subagent handle list a child of this session shares (R58): `None` for a root, `Some` for a
    /// child, so the whole tree hands out and collects one list. Without it a settlement names a
    /// handle no runner in `open` holds.
    pub(crate) tree_slots: Option<TaskSlots>,
    /// The head's answers, as the whole tree reaches them: a child has no head, so its gate's card is
    /// posted to the tree's ROOT and settled by that session's `Answers`, or nothing could answer it.
    /// `None` for a root, whose own are built at open.
    pub tree_head: Option<std::sync::Arc<crate::answers::Answers>>,
    /// The daemon's review desk: who is reviewing what, and what they have handed in. It rides
    /// [`Parts`] because a review has two sides in two sessions, and one `Arc` is what makes them one
    /// act. Not a store: the durable row is the queue's own.
    pub reviews: std::sync::Arc<letibot_tools::builtins::verdict::ReviewDesk>,
}

/// How many ledger rows a resume announces between progress ticks: a tick per row overflowed a
/// head's queue and demoted it, so the restore's own progress bar was killed by the events carrying it.
const FILLING_STRIDE: usize = 64;

/// How many transcript rows one store transaction writes, and it is the store's own chunk: one
/// chunk here is exactly one `BEGIN IMMEDIATE`, so a committed chunk, a tick of the carry's
/// `Filling` line and the `persisted` count all mean the same rows.
const PERSIST_CHUNK: usize = letibot_tokencore::store::Store::APPEND_CHUNK;

impl Parts {
    pub fn load(cfg: &Config) -> Result<Parts, HarnessError> {
        let wiring = cfg.dialect.wiring(cfg.effort.as_deref());
        // Which vocabulary, decided by what the turns need: a GGUF is read by llama.cpp, and no GGUF
        // with a provider means the byte vocabulary, whose ids never leave this machine. No GGUF and no
        // provider is the same byte vocabulary and no refusal: a turn on the local route is what is.
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
        let skills = std::sync::Arc::new(
            letibot_tools::builtins::skill::SkillRegistry::load_for(&cfg.skill_dirs())
                .with_pins(cfg.skills_pin().iter().cloned()),
        );
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
            // The daemon-wide board map, over the same store the sessions write - see the
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
    /// The model went round the tool loop `max_tool_rounds` times and was still producing results:
    /// the backstop, not a judgement about the work, and reported rather than silently truncated to
    /// whatever the last round happened to say.
    LoopBound {
        rounds: usize,
    },
    /// The turn stopped because the next round would not fit the window: not a failure of the work,
    /// since everything produced is committed and `Sessions` compacts before the next turn.
    /// `resident` and `window` are in `unit`, and `ledger` keeps the box's own pair for the parenthesis.
    ContextWall {
        rounds: usize,
        resident: u64,
        window: u64,
        unit: String,
        ledger: (u64, u64),
    },
    /// The progress check fired: `stall_rounds` consecutive rounds produced nothing this turn had not
    /// already seen. `evidence` names what the detector saw, because a stop that cites evidence is one
    /// an operator can contradict and a round count is not.
    NoProgress {
        evidence: String,
    },
    /// The endpoint failed, and this is the whole account of it: the class, how many times the call
    /// was tried and for how long, and the provider's own last words. `cause` stays typed because two
    /// questions are asked of a failure after it is said, and nothing about it is secret.
    Endpoint {
        said: String,
        cause: letibot_turn::HttpError,
    },
}

impl HarnessError {
    /// **The HTTP failure underneath this error**, for the two questions asked after it is
    /// said - never for re-deciding whether to retry, which the ladder already answered.
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
            // Never "the model called tools N times without answering": it WAS answering and had not
            // finished, and reaching here means every round still produced new results, which is a fact
            // about the size of the task and not about the model.
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
    /// §5.7: the output was cut short but is usable. Never folded into success.
    pub truncated: bool,
}

impl Reply {
    /// D11's `f_keep` (`cached / what the previous turn left in the cache`) for every submission this
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

/// One thing that was said in this session, with **who said it** recorded at the
/// moment it was appended.
struct Said {
    speaker: Speaker,
    /// **At most [`UTTERANCE_CHARS`] characters** - see [`TrailMirror::say`].
    text: String,
    /// It was longer than that when it was said, and says so - the flag is what
    /// `Utterance::clipped` reports, and it is recorded here rather than recomputed
    /// because the text this struct holds is already the clipped one.
    clipped: bool,
    /// Which turn of this session it belongs to.
    turn: u32,
    /// When it was said. `None` for a row rebuilt from the store - see
    /// [`TrailMirror`].
    at: Option<Instant>,
}

/// The authorisation trail's source, and why it is a mirror rather than a walk over `session.items`:
/// every `User` row looks identical in the transcript, so calling them all the operator's would let
/// the harness authorise itself. The provenance is known only at the append, so it is recorded there.
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
    /// The agent's most recent statement of what it is doing, for the guard to connect the operator's
    /// words to a call with. Never an authorisation and never citable. One line, replaced rather than
    /// accumulated, because the question is what it is doing NOW.
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

    /// The denominator: every item this session appended, of any kind. Kept apart from
    /// [`TrailMirror::say`] because collapsing the two is the defect `TrailProvenance` exists to stop:
    /// `0 of 41 messages` is a measurement and `0` is not.
    fn note_items(&self, n: usize) {
        self.lock().items += n;
    }

    /// Record one thing that was said, with the provenance only the caller knows. Two caps, and both
    /// were already the only values anything read (`trail()` takes the last sixty and clips each to six
    /// hundred), so this is a memory bound and not a behaviour change.
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

    /// Drop everything past the [`TRAIL_SCAN`] the walk can reach: one statement in one place, because
    /// `say` and `seed` must not disagree about how long the trail is.
    fn keep_the_last_scan(said: &mut Vec<Said>) {
        if said.len() > TRAIL_SCAN {
            let drop = said.len() - TRAIL_SCAN;
            said.drain(..drop);
        }
    }

    /// **What the trail holds**, in bytes of text and in utterances - the third term of a
    /// hub's [`Footprint`](letibot_sessionlog::hub::Footprint), read live because this is
    /// already shared behind its own lock.
    pub(crate) fn held(&self) -> (usize, usize) {
        let g = self.lock();
        (g.said.iter().map(|s| s.text.len()).sum(), g.said.len())
    }

    /// Seed from a transcript rebuilt out of the store. The speaker comes off the row (R42): calling
    /// every `User` row the operator's put the session's own settlements and notices into the
    /// authorisation trail as citable utterances. A row older than the field reads as `Operator`.
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
                // The same two caps a live utterance gets - see `say`. A resumed session's
                // rows are the same text by the same rule, so a seeded trail and a live one
                // are one bound.
                let clipped = text.chars().count() > UTTERANCE_CHARS;
                let text: String = if clipped {
                    text.chars().take(UTTERANCE_CHARS).collect()
                } else {
                    text
                };
                g.said.push(Said {
                    // The two `Speaker` enums meet here and nowhere else, matched inline rather than
                    // behind a `From`, so the one place they touch is the one place a reader looks for
                    // the mapping.
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

    /// Record it. Called with the assistant's own prose for a turn - the sentence
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
        // The denominator is what was LOOKED AT, not what came back: deriving it from the result
        // collapses "I looked at 41 messages and 0 were the operator's" into "I looked at 0".
        AuthorisationTrail::from_messages(utterances, scanned.max(g.items.min(TRAIL_SCAN)))
    }
}

// ---------------------------------------------------------------------------
// A question the model asked, and the answer a person gave it
// ---------------------------------------------------------------------------

/// The tool whose call is an exchange with a person rather than a piece of work. Spelled here and
/// not imported, because at this layer the name means *the one call whose result carries a person's
/// own utterance*, a fact about the transcript and not about the schema.
const ASK_USER_QUESTION: &str = "ask_user_question";

/// The person's own words, exactly: what their row carries, or `None` when there is no honest way to
/// state them. The parts are read off the ANSWER and nothing of ours is prepended, because a citable
/// utterance with a harness sentence glued to it is a citable utterance of the wrong thing.
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

/// The question, as the SESSION's own row and never the operator's: the trail would otherwise quote
/// it back as authorisation for the answer to it. The options are a list, one per line, so an option
/// holding ` | ` is not read as two.
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

/// The two rows the person's half of a question makes, as a free function so the shape is testable
/// without a daemon, a model or a socket. The order is the question and then the answer; where they
/// land beside the call's own result is `Harness::settle_asked`'s business.
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

/// Every refusal, onto the session's own log, the moment it is decided: the operator otherwise learns
/// of a denial by noticing a task that stopped. It publishes rather than returning, because the gate
/// calls it from inside the tool path and the operator may be somewhere else entirely.
pub struct HubDenials {
    hub: Arc<Hub>,
}

impl HubDenials {
    pub fn new(hub: Arc<Hub>) -> Self {
        HubDenials { hub }
    }
}

/// Where a gate admission goes: the channel in the other direction from [`HubDenials`], and a seam
/// because an admission is not a denial and must not be drawn as one. It collects the sentence and
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
                 {}), the row and `/gate` carry the full reason",
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
    fn denied(&self, notice: &DenialNotice) -> usize {
        // Read before publishing: a head that is not attached when the refusal is raised has
        // not been told, whatever it later finds in the snapshot.
        let heads = self.hub.attached_heads();
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
        heads
    }
}

// ---------------------------------------------------------------------------
// Steering
// ---------------------------------------------------------------------------

/// Steering from the head socket, the intent check, a fired monitor and a settlement: the step
/// boundary is the only place any of them can be injected, so they share one source. Provenance is
/// recorded here, because after the engine appends them they are four identical `User` items.
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
    /// **This session's results, delivered by R79's rules** - the immediates at every round
    /// boundary, the held batch only past the starvation cap. `None` in a test that has no
    /// daemon behind it.
    deliveries: Option<Deliveries>,
    /// **This session's children** - the tree's downward edge. An `Interrupt` that reaches a
    /// running turn IS this session being stopped, and a supervisor stops its children before
    /// it goes (see [`HarnessTaskRunner`]). `None` in a test with no session behind it.
    subagents: Option<Arc<dyn letibot_tools::builtins::task::TaskRunner>>,
    /// Messages handed back by a failed turn, drained before the hub's queue so the next poll finds
    /// them first. Shared with the harness on purpose: a `HubSteering` is built per round, and one
    /// that owned them lost them when the round let go.
    held: Arc<Mutex<VecDeque<SteeringMessage>>>,
    /// **An interrupt this turn took**, shared with the harness the way `held` is: what the turn's
    /// ending is told, so a stop does not ring a wake of its own a moment after the operator's.
    stopped: Arc<AtomicBool>,
    /// **This session's board**, for the one command a running turn applies to it: the
    /// operator's answer to a row's question (`CommandKind::AnswerTodo`), which reaches the model
    /// at the next step boundary rather than after the turn. `None` in a test with no board.
    todos: Option<Arc<TodoBoard>>,
    /// This session's own parent, if it has one: the one fact `message_from_a_session` needs to word
    /// a message correctly. A message says "your parent session" only when the sender IS this
    /// session's parent, and the between-turns door reads the same field off the config.
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
            stopped: Default::default(),
            todos: None,
            parent: None,
        }
    }
}

/// The warning code for a session whose workspace no longer exists (R92.24).
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

/// **"workspace gone", in one sentence** - said when a session opens or wakes on a workspace that
/// no longer exists (R92.24).
fn workspace_gone(workspace: &std::path::Path) -> String {
    format!(
        "workspace gone: `{}` no longer exists (a retired worktree, or a checkout moved or \
         removed), so nothing this session runs can work there",
        workspace.display()
    )
}

/// A monitor firing, as the sentence the model is given, or `None` when nothing in `fired` is news.
/// It reports WHY it fired, one line per watch. The model's own `retire` and an owner ending are
/// recorded but not announced: the first is its act one call ago, the second is the scope going away.
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

/// The firings `monitor_notice` was composed from, as the facts the `notice` hook is handed, so a
/// layer can build a different sentence out of the same news rather than reword this one.
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

/// **How many ended monitors the pane lists** - the newest. A session that ran a repeating watch
/// all day would otherwise ship its whole history with every job-table refill.
pub const MONITORS_ENDED_SHOWN: usize = 20;

/// **What reaps a monitor, as a person says it** - `session`, `turn`, or a named scope's own name;
/// the scope's id (`session.s0`) is the daemon's, not the reader's.
fn owner_words(s: &letibot_tools::exec::scope::ScopeId) -> String {
    use letibot_tools::exec::scope::ScopeKind;
    match s.kind {
        ScopeKind::Session => "session".into(),
        ScopeKind::Turn => "turn".into(),
        ScopeKind::Explicit => format!("scope `{}`", s.name),
    }
}

/// **Who armed a monitor, as a person says it** - `turn pr-watch` was the harness on a
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

    /// **The monitors pane's rows** - a live repeating watch is one row with its firing count and
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

/// Who `monitor`'s own `retire` op says retired a watch (`builtins::monitor`) - the model's act,
/// which [`monitor_notice`] does not announce back to it.
const MODEL_RETIRE: &str = "the model";

/// Who a retire from the monitors pane says retired a watch - announced to the model, unlike its
/// own ([`MODEL_RETIRE`]).
const OPERATOR_RETIRE: &str = "the operator";

/// The completion of a background job, as the sentence the model is handed: a labelled notice in the
/// harness's own voice. It names the job, its command, how it ended and where its output is, and never
/// inlines the output, which is one `job_output` away. `mine` says whose job it was.
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
        // ANOTHER session started - the model wrote its own one call ago, and echoing it back in
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
        // The one job whose result IS its output: a fanout's per-host table, carried in `detail`
        // rather than read from a stream, because the job has usually been reaped by now.
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

/// The same sentence for a subagent, and the same rule: a subagent settles through the job channel
/// because it IS one, and only the verb differs, since a child is read with `task_result`. A
/// handed-up child is still collectable here, because every level shares the tree's handle list.
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
        // **The child by its name**, `fix-login · s-…-sub-3` - the job rule, and the same
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

/// **The closing a `[job]` notice for this session's own jobs ends with**, up to its handles - the
/// words the head's fold and job card recognise (`tui`'s `PROMISES` and `CLOSINGS`).
pub const JOB_CLOSING: &str = "Output: `job_output` ";
/// The same for a `[task]` notice of this session's own children.
pub const TASK_CLOSING: &str = "Full answer: `task_result` ";

/// `job="j1", job="j2"` - the real handles, never a placeholder.
fn handles_as(field: &str, done: &[JobCompletion]) -> String {
    done.iter()
        .map(|c| format!("{field}=\"{}\"", c.job))
        .collect::<Vec<_>>()
        .join(", ")
}
/// One channel, two nouns: a job and a subagent arrive through the same queue and ring the same bell,
/// and are told apart here rather than in two queues, because two queues would be the second mechanism
/// the ruling avoided. `owner != me` is what says whose a settlement was.
fn completion_notices(done: Vec<JobCompletion>, me: &str, rows: &[TodoItem]) -> Vec<String> {
    // **Mine first**, then what was handed up - so a session reading one wake reads its
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

/// The plan rows that wait on these completions, as one line under the notice, or nothing when no row
/// does: a row attached with `wait_on` is the plan's own statement that this ending is what it waited
/// for. One line, opening with [`WAITING_ON_THIS`] or [`WAITING_ON_THESE`], naming open rows only.
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

/// The endings these rows wait on that the model has not digested yet, added to `jobs`: the half of
/// `running_jobs` that reads the watcher set's read record. A job already called running is skipped,
/// because running is its own quiet and the grace is for after.
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

/// The opening words of [`waiting_on_these`]' line for one completion - what the head's fold
/// (`tui`'s `folded_notice`) and job card recognise the line by.
pub const WAITING_ON_THIS: &str = "Waiting on this: ";
/// The same for a group of several completions.
pub const WAITING_ON_THESE: &str = "Waiting on these: ";

/// **The held batch, as one note** (R79): the `[meanwhile]` line, then the settlements in
/// [`completion_notices`]' own words - unchanged, so the head's compact job rows read the groups
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

/// R79's delivery, for the two places that hand a result to the model: the round boundary and a
/// turn's start. The decision is `crate::delivery::decide`'s, pure and asserted there; this reads the
/// session and turns the decision into notices, holding no state of its own.
#[derive(Clone)]
pub(crate) struct Deliveries {
    watch: Arc<JobWatchers>,
    /// The plan the decision reads. `None` in a test with no board.
    todos: Option<Arc<TodoBoard>>,
    log: Arc<crate::delivery::DeliveryLog>,
    cap: std::time::Duration,
    me: String,
}

/// What a between-turns read hands a turn's start: the notices said now (immediates and idle
/// arrivals, in `completion_notices`' words) and the `[meanwhile]` batch, apart so a wake can
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
        // **Delivered, and recorded as such** - the fact the plan check's grace reads
        // (`JobWatchers::grace_left`).
        self.watch.read_now(&done);
        completion_notices(done, &self.me, &rows)
    }

    /// The board the notices name rows from - empty in a test with no board.
    fn rows(&self) -> Vec<TodoItem> {
        self.todos
            .as_ref()
            .map(|t| t.snapshot())
            .unwrap_or_default()
    }

    /// **The held batch, taken and recorded**, as one `[meanwhile]` note - or `None` when nothing
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

    /// A turn the model is taking anyway: whatever is immediate, what arrived while the session was
    /// idle, and the batch held while the last turn ran (under `Rule::Batch`, which rides this turn).
    /// Test-only: every door that opens a turn uses [`Deliveries::at_turn_start_unread`], because a
    /// door's row can still fail (R91.11).
    #[cfg(test)]
    pub(crate) fn at_turn_start(&self) -> Option<String> {
        let b = self.between_turns(|_| false, crate::delivery::Rule::Batch);
        let mut parts = b.now;
        parts.extend(b.batch);
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// [`Deliveries::at_turn_start`] for a door whose submit can still fail: the words, and the
    /// results they say, for a caller that marks them only once its row is in (R91.11).
    pub(crate) fn at_turn_start_unread(&self) -> (Option<String>, Vec<JobCompletion>) {
        let (b, told) = self.between_turns_unread(|_| false, crate::delivery::Rule::Batch);
        let mut parts = b.now;
        parts.extend(b.batch);
        ((!parts.is_empty()).then(|| parts.join("\n\n")), told)
    }

    /// Everything owed to a session between turns: holding exists only so a running turn is not
    /// interrupted, so nothing stays held. What arrived while the model was working is the batch under
    /// `held_rule`; what arrived while it was idle is an ordinary completion, recorded as `Rule::Idle`.
    /// Test-only, for [`Deliveries::at_turn_start`]'s reason: a door waits until its row is in (R91.11).
    #[cfg(test)]
    pub(crate) fn between_turns(
        &self,
        skip: impl Fn(&JobCompletion) -> bool,
        held_rule: crate::delivery::Rule,
    ) -> BetweenTurns {
        let (out, told) = self.between_turns_unread(skip, held_rule);
        self.read(&told);
        out
    }

    /// `between_turns` without marking anything read (R91.11): the sentences and the results they say,
    /// for a caller that has not yet put them in front of the model. It marks them with `read` once it
    /// has, or hands them back with `put_back` when the turn never opened.
    pub(crate) fn between_turns_unread(
        &self,
        skip: impl Fn(&JobCompletion) -> bool,
        held_rule: crate::delivery::Rule,
    ) -> (BetweenTurns, Vec<JobCompletion>) {
        // **Taken means owed until the turn that says it begins** - marked before the take,
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

    /// **These results are in front of the model now** - the read record the plan check's grace
    /// reads, and a subagent's `delivered` in the store.
    pub(crate) fn read(&self, told: &[JobCompletion]) {
        if !told.is_empty() {
            self.watch.read_now(told);
        }
    }

    /// **These results were never told** - the turn that was to carry them did not open - so
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
        // A handed-back message comes out first, with the flags it had when it was taken: it was taken
        // before anything still in the hub's queue, so a later message cannot reorder an interrupt. It
        // is not recorded in the trail here, because `try_next` recorded it the first time it was taken.
        if let Some(m) = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        {
            return Some(m);
        }
        // The head first: somebody is waiting on it. Steering-scoped, so a
        // compaction queued behind this turn stays queued for the worker - see
        // [`Hub::try_steering_command`].
        if let Some(cmd) = self.hub.try_steering_command() {
            return match cmd.kind {
                CommandKind::Prompt { text, attachments } => {
                    // The operator, typing into a running turn: the one utterance that can authorise the
                    // action it is racing. The chips are read HERE and not by the engine, because a turn's
                    // library has no business opening the operator's files.
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
                    // The children first, then this session: the one door an interrupt reaches a turn in
                    // flight through. Nothing here waits for the children, because a stop that blocked on
                    // its subtree would hold the turn it is stopping open.
                    stop_children_first(self.subagents.as_ref(), &self.hub);
                    // And the turn's ending is told: a session the operator stopped must not ring a wake
                    // for the results it held, which is a turn nobody asked for.
                    self.stopped.store(true, Ordering::SeqCst);
                    Some(SteeringMessage::urgent(reason))
                }
                CommandKind::Message { from, text } => {
                    // A parent's live correction is an agent's utterance, so it is recorded as
                    // `Speaker::Agent`: an operator's words can authorise and be taken back, a parent's
                    // cannot. It arrives at the next round boundary, as context rather than an interrupt.
                    let said = message_from_a_session(&from, &text, self.parent.as_deref());
                    if let Some(t) = &self.trail {
                        t.say(Speaker::Agent, &said, Some(Instant::now()));
                    }
                    Some(SteeringMessage::from_a_session(said, from))
                }
                // The operator answered a row's question while the turn runs: an operator's words are
                // immediate, so the answer is applied to the board HERE and the model reads it at this
                // step boundary, as a notice that stands alone and a take-back cannot drop.
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
        // Then the room, at the boundary a running turn actually reaches. A room event becomes a
        // sentence on the connection's thread and `wake` reads it between turns, which is the wrong door
        // for a long turn: the note arrives after the work it would have changed. One queue, so once.
        let room = crate::room::wake::take(&self.hub.session_id());
        if !room.is_empty() {
            let text = room.join("\n\n");
            if let Some(t) = &self.trail {
                t.say(Speaker::Agent, &text, Some(Instant::now()));
            }
            return Some(SteeringMessage::normal(text));
        }
        // Then a background job or subagent of THIS session's own that has ended, above the monitors
        // because that block returns early with no registry, and a read-only seat with `task` and no
        // monitors is exactly this case. Only what the work waits on comes in mid-turn.
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
        // A firing is immediate by R79's rules and recorded as one - every one, even those
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

    /// The take-back the operator issued from a head: the hub drops the head's still-queued prompts and
    /// the engine's `Pending` drops the held text when this returns true. Both halves are one recall  -
    /// one prompt is in the hub's queue, another is already held here.
    fn try_withdraw(&mut self) -> bool {
        self.hub.try_withdraw_command()
    }

    /// Hand a message back to the source, for a turn that failed: the words go back rather than being
    /// appended, because appending on an error path advances the ledger and the prefix. No `t.say` here,
    /// because `try_next` already recorded it; the message keeps its flags, being the same value.
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
    /// The right to append to this session's transcript, claimed at open and given back on drop. A field
    /// because the claim is a LIFETIME and not a permission check: it is what a second process's open is
    /// refused by. `None` with no store, and for a session whose live copy is elsewhere.
    _writer: Option<crate::writer::Claim>,
    /// **The daemon's mode rows**, which a `/mode` taken by a running turn writes
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
    /// **The skills this daemon loaded**, which the prompt's index is composed from and the page's
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
    /// The turn this harness last started. §4.5: a failure has to name one.
    last_turn_id: String,
    /// Read at open time from the backend, the gate and the seated schemas, so the
    /// adjudication disclosure is a reading rather than a claim. See [`GateWiring`].
    wiring: GateWiring,
    /// True only while [`Harness::compact`] is running its summary turn. See the
    /// context-wall check in `run_rounds` for why that turn must be exempt.
    compacting: bool,
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
    /// **The `!` sequence**: how many of the operator's own shell lines this session
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
    /// **The row the last plan-check named**, so the check keeps asking about THAT row rather than
    /// whatever happens to head the queue now. See `unfinished_plan_for`: the choice is sticky, and
    /// this is the one field that remembers it.
    nag_choice: Option<String>,
    /// The stale-prompt sentence the model has already been told, so it is told ONCE. The decision is
    /// the person's and it stares at them from their own head. Compared by text, so a different
    /// sentence is told again, and cleared by the fork that re-seats.
    stale_told: Option<String>,
    /// **A fork whose rows are not all stored yet** (R91.2) - sealed by [`Harness::seal_fork`]
    /// once they are.
    unsealed_fork: Option<UnsealedFork>,
    /// **This turn's metrics, as it runs** (TODO R81) - begun and closed by the turn tail, fed
    /// by the round loop. See `crate::instruments`.
    pub(crate) meter: crate::instruments::TurnMeter,
    /// T21.3's encoder and error signal. Always constructed and always attached  -
    /// measuring costs nothing and `Verification::NoEncoder` is a state whose
    /// reason is ours rather than the model's.
    intent: Arc<IntentLedger>,
    /// What the next step boundary will inject: intent findings, monitor firings.
    /// Shared with the session's [`HubSteering`].
    injected: Arc<Mutex<VecDeque<String>>>,
    /// What a round handed back to its steering, shared with every `HubSteering` this harness builds,
    /// so a message handed back by a failed or interrupted round outlives the round (R91.1).
    handed_back: Arc<Mutex<VecDeque<SteeringMessage>>>,
    /// **Whether the turn in flight was stopped by an interrupt**, shared with every `HubSteering` for
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
    /// Whether this harness has said its workspace is gone (R92.24) - once, at the first wake
    /// that found it so.
    workspace_gone_said: bool,
    /// The monitor names live at the last [`Harness::sync_monitor_records`], sorted.
    monitors_seen_live: Vec<String>,
    /// The queue entries this session's model has been told about, so each is said once per daemon. One
    /// set for both kinds, because an entry has one `evidence` string and a second telling would be
    /// about the same row.
    gate_told: std::collections::HashSet<String>,
    /// **The outage the retry ladder is in, if any** - one per endpoint, told ONCE when it ends.
    /// See [`Outage`] for why the row is written at the end and not per attempt.
    outage: Option<Outage>,
    /// The cloud provider the turns go to, when the session has one. `None` is
    /// the local server through the engine's own `/completion` path - **unless**
    /// [`Harness::key_wanted`] is set, which is a provider with no key yet.
    provider: Option<Box<dyn letibot_backend::MessagesBackend>>,
    /// A provider chosen and not yet usable, because no key for it resolves. It used to be a refusal at
    /// open, which killed the daemon before a head could attach; now the first turn asks for the key on
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
    /// **This turn IS the automatic continuation a fallback earned** - set while it runs, so
    /// [`Harness::fall_back_from`] moves the session to the next name without arming a second
    /// continuation. See `fallback_continuation` for why the bound exists.
    continuation_of_fallback: bool,
    /// The session's plugin host, once the daemon has one: the same live `Arc` the `plugin` tool asks
    /// through, handed over because the tool is seated at open and the host when the daemon opens the
    /// session. `None` in a session the daemon did not open, where scaffold and build still work.
    plugin_slot: Arc<std::sync::Mutex<Option<Arc<crate::plugins::PluginHost>>>>,
    /// **The mode confirmation a head asked for and nobody has answered**
    /// (`PROTOCOL_VERSION` 54, B8). One per session: a second ask supersedes it.
    mode_confirm: Option<PendingModeConfirm>,
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

    /// This session's connection to the box's room, when it has one: the `room` tool's half, and what a
    /// room pane is drawn from. `None` on a box with no coordinator and on a subagent, which speaks
    /// through its parent, and both are ordinary states rather than failures.
    pub fn room_link(&self) -> Option<Arc<crate::room::link::SessionRoom>> {
        self.room.clone()
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
        // And the head is told, including when the record could not be written: this used to be a hub
        // block that did nothing and a `let _ =` on the append, so a close-out the store refused was
        // announced to nobody and the spinner the sweep exists to end stayed up.
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
            // **The same `mode_set` line the `/mode` path writes**, because a point that moved
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
        // **Whether the board waits on the operator** - `none`, or how many and the first
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

    /// **A warning on this session's log** - the operator's own sentence about something that
    /// did not go as asked. One writer for the two doors that can refuse an attachment.
    pub(crate) fn warn(&self, code: &str, detail: String) {
        self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
            code: code.into(),
            detail,
            compaction: None,
        });
    }

    /// **The batch a turn about to open carries** (R79), taken and NOT yet called delivered: the
    /// door marks it once its row is in the transcript, or hands it back with
    /// [`Harness::the_batch_arrived`] (R91.11).
    pub(crate) fn batch_riding_in(&self) -> (Option<String>, Vec<JobCompletion>) {
        match self.deliveries() {
            Some(d) => d.at_turn_start_unread(),
            None => (None, Vec::new()),
        }
    }

    /// **That batch reached the model, or it did not**: `opened` is whether the door's row went in.
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

    /// **R79's delivery for this session** - see [`Deliveries`]. `None` for a session with no
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

    /// **The operator ends a monitor** - the monitors pane's `x`, as `/monitor retire NAME`. The
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
        // **The box's instruction directories, from the config and not from `$HOME`**: the pane
        // re-reads the corpus on every open and the answer is the config's, so a fixture's pane
        // names the files a fixture wrote.
        let box_dirs = self.cfg.box_dirs.clone();
        // The vocabulary the section itself is measured with: a field of its own rather than the
        // engine's, because the pane's read happens on the server's thread and must not measure with the
        // vocabulary the session OPENED on.
        let vocab = self.note_vocab.clone();
        self.session_registry.set_notes_source(
            &self.cfg.session_id,
            std::sync::Arc::new(move || {
                crate::standing_notes::rows(&workspace, &global, &box_dirs, &vocab)
            }),
        );
    }

    /// **Install this session's skills source**, which is what a head's `ListSkills` is answered
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

    /// **The waiter was asked for and could not be started.** The caller still publishes the
    /// `Warning` with the error; this is the banner's half, which from a bool read exactly like a
    /// session nobody armed - and a reader told *POLL ONLY* goes looking for a knob that would not
    /// have helped.
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

    /// Whether the queues' news is this daemon's to tell a model: false under `--no-queue`, and that is
    /// the whole of the model-facing half of the flag. Every notice would be about work another daemon
    /// owns. The heads still draw the queue's alerts; only the model's copy is withheld.
    fn queue_news_is_mine(&self) -> bool {
        self.cfg.serve_queues
    }

    /// This project's stuck rows, as the idle check reads them: `None` when this session is not the
    /// project's, when the queue is not this daemon's to speak about, or when nothing is stuck. The
    /// cached resolution, because this is read on every turn's end and one workspace does not change.
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

    /// What the idle check's clock needs to know about this project's queue: one reading, so the
    /// clock's arming, the sentence and the identity cannot disagree. `says` is `Stuck::is_news` and
    /// not *there are bad rows*, because a row that just went bad is the transition notice's.
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

    /// The census as a sentence, when there is one to say: `None` for a queue with nothing stuck, or
    /// one whose oldest row is too young to be news. Composed from the store at the moment it is said,
    /// so a row that recovered between arming and delivery is not announced as stuck.
    pub fn stuck_census_text(&self) -> Option<String> {
        let now = letibot_sessionlog::registry::now_ms();
        let census = self.stuck_census()?;
        census
            .is_news(now)
            .then(|| crate::mergequeue::notices::stuck_census(&census, now))
    }

    /// The census as a TURN: the sentence `stuck_census_text` composed, said by the harness and answered
    /// by the model. As a notice it reached nobody for hours, because a mailbox is read at the model's next
    /// turn and a quiet session has none. The held batch rides in with it, as R79 gives it to any break.
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

    /// A target emergency, told to the model of the project that has to act: the queue's warnings are
    /// drawn by heads and read by no model, so a ROOT session is told once per daemon, with the failing
    /// step and its output first. A branch handed over under `merge_via_session` is `handover_notices`'s.
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
        // The repository this session is the project of - read only when there is news.
        // Every repository of the workspace - one, or each of a one-org project folder's.
        let mine = crate::mergequeue::repositories_of_workspace(&self.cfg.workspace);
        let mut out = Vec::new();
        // **Green again** (R92.6): an emergency this session was told about is over - said once,
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
    /// the project gates and merges the branch itself - told once, read off the row.
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

    /// The reviews this daemon's queues claimed, served by THIS session's runner: the review queue's
    /// session is the host and every other session serves nothing. It starts a reviewer per claimed row,
    /// writes the verdict it handed in, counts an attempt that reached none, and kills a stale one.
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

    /// **Whether this session speaks in the box's room** - the daemon's own question, and the one
    /// the room pass's clock asks before it makes itself a reason to wake.
    pub fn has_room(&self) -> bool {
        self.room.is_some()
    }

    /// Whose subagent this session is, as the open settled it: resolved once by `Registry::parent_for`,
    /// so a child stays a child for the life of the process and the room's door, the settings row and
    /// the store row cannot disagree.
    pub fn parent_session(&self) -> Option<&str> {
        self.cfg.parent_session_id.as_deref()
    }

    /// The room's reminders, onto this session's own board: one row per item it holds, carried by the
    /// idle check so a holder is reminded without a model call. A subagent returns 0 first, because the
    /// room's rule is top-level sessions only. A room that does not answer clears the rows.
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

    /// Whether this harness is the review queue's own session - the one
    /// [`crate::sessions::gatekeeper::session`] names for this daemon's workspace, whose board is
    /// the queue.
    fn is_the_queue(&self) -> bool {
        self.hub.session_id() == crate::sessions::gatekeeper::session(&self.cfg.workspace)
    }

    /// **The review queue, onto this session's board** - one row per review this daemon's queues
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

    /// **The repository a `/queue` verb may act in** (R92 item 5) - `None` is every repository.
    /// [`QueueScope::Here`](crate::slash::QueueScope::Here) is this session's own, and a session
    /// of no repository is told so with the line that reaches every repository.
    pub fn queue_scope(&self, scope: &crate::slash::QueueScope) -> Result<Option<String>, String> {
        crate::mergequeue::verbs::scope_repository(&self.cfg.workspace, scope)
    }

    /// **[`Harness::queue_scope`] for a session of several repositories** - see
    /// [`crate::mergequeue::verbs::scope_repositories`].
    pub fn queue_scopes(
        &self,
        scope: &crate::slash::QueueScope,
    ) -> Result<Vec<Option<String>>, String> {
        crate::mergequeue::verbs::scope_repositories(&self.cfg.workspace, scope)
    }

    /// **Which of `scopes` holds the row `id`** - the first when there is one scope (the verb
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

    /// `/queue restart ID` - a parked review back to waiting, a parked landing back in line.
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

    /// `/queue approve ID` - the operator's approval over the reviewer (decision 5).
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

    /// `/queue veto ID WHY` - the operator's rejection, delivered to the child that did the work.
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

    /// `/queue rm ID` - withdrawn, with the history kept.
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

    /// `/queue reset [ID]` - every parked row of `repository` restarted, or the one named.
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

    /// `/queue clean [ID]` - every parked row of `repository` withdrawn, or the one named.
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

    /// **The repository this session's queue verbs are about, and its target** - the one its
    /// workspace means (`mergequeue::repo_of_workspace`), keyed the way the queues key it, unless
    /// `named` is a path; and `[queue] target` (else `main`/`master`) when no target is named.
    pub fn queue_repository(
        &self,
        named: Option<&str>,
        target: Option<&str>,
    ) -> Result<(String, String), String> {
        crate::mergequeue::desk::resolve(&self.cfg.workspace, named, target)
    }

    /// `/queue regate [/REPO] [TARGET]` and the `queue` tool's `regate` - the target's tip gated
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
        // **Only when the live set moved** - a wake is frequent and a store open is not free, and
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

/// A window onto a background job's output, with the numbers a pane needs for its own header and
/// paging. `job_output` returns prose for a head that prints what it is handed; this is the same read
/// with the offsets beside the text, so the pane need not parse a sentence it did not write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobWindow {
    /// The offset this window actually starts at - the request is clamped to what
    /// survives, not refused.
    pub from: u64,
    /// One past the window's last byte.
    pub to: u64,
    /// Everything the job has written, ever. The denominator.
    pub produced: u64,
    /// Bytes that fell off the front of the ring before this window: what a head
    /// must disclose so a window's start is not read as the job's start.
    pub dropped: u64,
    /// The job's own word - `exited 0`, `running`, `killed by …`.
    pub state: String,
    /// **Whether anything was ever executed for this job** - `true` only for the one
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

/// What the world says about one handle: three answers, because a reader acts differently on each.
/// `Unknown` MEETS the condition, since a job that is not running has ended and a handle this daemon
/// never heard of is what that looks like after a restart. It does not say what became of the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobFate {
    /// Still running **here**. The condition is not met.
    Running,
    /// It ended, and this is its own word - `exited 0`, `killed by job_kill`, `signalled 15`.
    Ended { state: String },
    /// **Not a job this session knows**: it ended before this daemon came back, or the handle is
    /// wrong. The condition is met and the result is not here.
    Unknown,
}

/// One due row, as the sentence the model is handed: pure, so the wording is asserted without a
/// session. `handle` is passed rather than carried in the fate, because a fate is a fact about a job
/// and the handle is how a row names it.
fn fate_line(handle: &str, fate: &JobFate) -> String {
    match fate {
        // Never printed - a running job is not due - and answerable anyway, because a function
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

/// The decision `Harness::job_fate` makes, as a function of what its two looks returned: pulled out so
/// the choice a conditioned row turns on is asserted without a backend, a registry, a gate and a store.
/// The ORDER is the whole of the honesty, since only the host can say a job runs NOW.
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

/// Which of a board's rows the world now meets: the filter and the pairing, apart from the harness. Four
/// skips, each a fact: no condition, a running job, a row the check may not speak about, and one already
/// done. `jobs` is handed in so this stays a function of its arguments.
fn due_rows(
    rows: &[TodoItem],
    children: &letibot_tools::builtins::todo::ChildSessions,
    jobs: &letibot_tools::builtins::todo::RunningJobs,
    fate: impl Fn(&str) -> JobFate,
) -> Vec<(String, String, JobFate)> {
    // A firing is a delivery, not a nag: the grace after an ending keeps the CHECK from asking about a
    // row the model has not digested the news for, and this IS that news, so it is read without the grace.
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

/// What a recorded job's window says, apart from the harness that found the row: a free function, so the
/// words can be asserted without a store, a vocabulary or a session.
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
    /// The window for a handle this daemon never ran, or the refusal when the store has never heard of it
    /// either. It says *not in this table* rather than *it ran*, because the row carries a state word and
    /// this does not parse it. The bytes are gone; the row's `redirect` says where they went instead.
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

    /// **The stored row for one handle**, or `None` - the one read the output window and the
    /// condition evaluator share, so the two cannot look in different places.
    fn recorded_job(&self, handle: &str) -> Option<JobRecord> {
        let store = self.store.as_ref()?;
        store
            .jobs(&self.cfg.session_id)
            .ok()?
            .into_iter()
            .find(|r| r.handle == handle)
    }

    /// What the world says about one handle. The live table is asked FIRST, which is the whole of the
    /// honesty: only the process host can say a job runs now, and a stored `running` row is a fact about
    /// then. The record is the second look, because it is what survives a restart.
    pub fn job_fate(&self, handle: &str) -> JobFate {
        // **Two looks, and [`fate_of`] is the decision between them** - the live table first,
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

    /// The rows whose condition the world now meets: the row's text, the handle and the fate, so the
    /// caller can report and consume them in one breath. The fate is carried and not collapsed into a
    /// boolean, because a handle that ended here can be read and one never heard of cannot.
    pub fn due_todo_rows(&self) -> Vec<(String, String, JobFate)> {
        // **The rows and both live facts, taken together** - the same reading `nag_notice` takes, so
        // the firing and the nag cannot disagree about whether a row is one the check may speak
        // about: a row waiting on a young child or on a running job is not due by EITHER door.
        let rows = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&rows);
        due_rows(&rows, &children, &jobs, |handle| self.job_fate(handle))
    }

    /// The same read as [`Self::job_output`], as a window rather than a page of
    /// prose - see [`JobWindow`] for why both exist.
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
            // A job this daemon never ran, but whose ending it may have written down: the process table is
            // memory, so after a restart the host answers nothing about a job the store has a row for. The
            // host is still asked first, since only it can say a job runs now.
            return self.recorded_job_window(job);
        };
        let slice = host
            .output(&jid, offset, limit)
            .map_err(|e| e.to_string())?;
        // The decision is `job_window_for`'s, apart from the two reads, for the reason `fate_of` is apart
        // from `job_fate`: this is what a pane turns on, and reaching it through a harness needs a backend,
        // a registry, a gate and a store, so nothing held it to a real job.
        Ok(job_window_for(
            &view.command,
            &view.state,
            &slice,
            offset,
            limit,
        ))
    }
}

/// One live read of a job's output, as the event that answers it: the half of `job_output_window` that
/// needs only the process host, so it can run on a head's connection thread while the worker runs a turn.
/// `None` when the host does not hold the job, whose answer is the store's record.
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

/// The window one read of a job's output answers with: the decision half of
/// [`Harness::job_output_window`], which does the two reads and hands everything to this.
///
/// A job whose command redirects its output to a file answers with the FILE, and the daemon does the
/// reading, because a head has no business reading the box's filesystem. The redirect is enough on its
/// own: reading from the file only when the capture was completely empty took a command as either
/// redirecting or writing, and a compound command does both.
///
/// ```text
/// handle  state      produced  redirect
/// j87     exited 0   2426      /tmp/tpcc-trace2.log   ← the file was NOT read
/// j82     killed     13        /tmp/tpcc-trace.log    ← same shape
/// j70     killed      0        /tmp/tpcc-trace-server.log ← this one worked
/// ```
///
/// So when a command names a file, that file is the window whatever the capture holds, and the
/// capture's bytes are not drawn. They are not lost silently either: the read says how many are
/// outside the window and where the window is. Splicing the two coordinate spaces together, or
/// switching to the file only at settlement, would make `produced`/`from`/`dropped` two stories
/// about one source.
///
/// `offset == 0` is the TAIL, the last `limit` bytes, because that is what an output pane is for.
/// Any other offset is a forward window from there, so a reader can walk to the end of a long log.
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
                // The preamble, counted rather than hidden: the capture may hold bytes the redirect never
                // took, and this window is over the file. The disclosure belongs on the fresh read, since
                // repeating the count on every paged read would be noise.
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
            // **Empty, and the redirected case cannot reach here** - a job whose command
            // redirects its output was answered from the FILE above.
            Vec::new()
        } else {
            progress_lines(&slice.text())
        },
        next: next_job_offset(slice),
    }
}

/// The tail of a file, as the window a pane draws: the file half of a redirected job. `offset == 0` is
/// the tail, any other offset a forward window, and `None` when the file cannot be read so the caller
/// falls back to the sentence naming it. Bytes, lossily decoded, because a log is not text.
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

/// The captured bytes, as lines a person can read, and a progress bar is one line: `\r` returns to
/// column 0 and what follows OVERWRITES it, so the line is each segment written over the last. The split
/// is the daemon's, so two heads cannot disagree about where a line ends.
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

/// Where a further read of a job's output would start, or `None` when the window has reached the end of
/// what the ring holds: `produced == dropped + retained`, so `to < produced` says there is more. A named
/// free function because it is the one piece of arithmetic in the window.
fn next_job_offset(slice: &letibot_tools::exec::OutputSlice) -> Option<u64> {
    (slice.to < slice.produced).then_some(slice.to)
}

/// What a job killed with its subagent's VM reads as, on its row.
const VM_LOST: &str = "lost with its VM";

/// **The background jobs a VM child left running, as its parent is told they died** (TODO.md
/// R92.1) - `None` when there were none. Pure, so the wording is asserted without a VM.
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

// The end-to-end test of the merge queue's enqueue, declared here because it tests THIS module's private
// runner, which a crate can only reach from inside itself.
mod clear;
pub use clear::{ClearReport, clear_reply};

// **A transcript whose rows do not rebuild under the prefix it names** - the repair to the fork
// that a store already in that state needs, so a conversation is not refused for ever. See the
// module's header for the shape it is, and why only that shape is repaired.
mod children;
// **A session's estate** - what it was still holding when it ended, what ended with it, and what
// it handed to the session that outlives it. See the module's header for why the substrate's own
// reaping is not the whole answer for a subagent.
mod estate;
pub use estate::{Estate, estate_said, settle_estate};
mod jobs;
mod models;
mod nags;
mod open;
mod opshell;
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

// **The branch is the deliverable** - the brief's rule checked when a worktree child's turn ends,
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
    // **The ceiling, when the operator named one** (R12). The default is the measured one;
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

    /// **A session above its repositories is told which ones it can name** - `task_start`'s
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

    /// **The keeper's brief begins with the mark**, on the gatekeeper's rule - so the piece that
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

/// What a D16 admission says on the transcript: the one line an operator reads while the work happens.
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
