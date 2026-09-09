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

use std::path::PathBuf;
use std::sync::Arc;

use letibot_dialect::StablePrefix;
use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::{LogSink, ToolLogSink};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_tokencore::{Vocab, ledger::hex as hex32};
use letibot_tools::{
    Gate, HostBackend, NoBoundary, Registry, ToolRuntime, Tool, roles,
};
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};
use letibot_turn::{
    Endpoint, EventSink, Session, SteeringMessage, SteeringSource, TurnEngine, TurnEvent,
    TurnFailure, TurnMetrics, TurnOk,
};

use crate::config::{Config, GateWiring, SpillPolicy, SpillStorage};
use crate::dialect::Wiring;
// `is_writable` is a trait method; the backend's own answer is only reachable
// with the trait in scope.
use letibot_tools::ExecBackend as _;

/// Everything the engine borrows for the life of the daemon.
///
/// Held apart from [`Harness`] because `TurnEngine<'a>` borrows the vocabulary and
/// the renderer, and a struct owning both the borrow and the borrowed would be
/// self-referential. The caller keeps this on the stack and hands out a reference,
/// which is a two-line inconvenience against a `Box::leak` that lies about
/// lifetimes.
pub struct Parts {
    pub vocab: Vocab,
    pub wiring: Wiring,
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
        Ok(Parts {
            vocab,
            wiring: cfg.dialect.wiring(cfg.effort.as_deref()),
        })
    }
}

#[derive(Debug)]
pub enum HarnessError {
    Setup(String),
    Turn(TurnFailure),
    Store(String),
    /// The model went round the tool loop `max_tool_rounds` times without
    /// producing an answer. Reported, never silently truncated to whatever the
    /// last round happened to say.
    LoopBound { rounds: usize },
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::Setup(s) => write!(f, "{s}"),
            HarnessError::Turn(e) => write!(f, "{e}"),
            HarnessError::Store(s) => write!(f, "store: {s}"),
            HarnessError::LoopBound { rounds } => write!(
                f,
                "the model called tools {rounds} times without answering; \
                 the loop bound stopped it"
            ),
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

/// Steering from the head socket (§5.8).
///
/// A `Prompt` that arrives while the model is generating is a correction, and it
/// goes in at the next step boundary. An `Interrupt` is the urgent form: it stops
/// generation at the next token and the partial output is kept.
///
/// The hub's queue is drained here *only while a turn is running*; between turns
/// the daemon's own worker owns it. One drainer at a time, by construction of who
/// is executing.
pub struct HubSteering {
    hub: Arc<Hub>,
}

impl HubSteering {
    pub fn new(hub: Arc<Hub>) -> Self {
        HubSteering { hub }
    }
}

impl SteeringSource for HubSteering {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        let cmd = self.hub.try_command()?;
        match cmd.kind {
            CommandKind::Prompt { text } => Some(SteeringMessage::normal(text)),
            CommandKind::Interrupt { reason } => Some(SteeringMessage::urgent(reason)),
            // An answer with no open decision. M1 has no adjudication, so there is
            // nothing that could have asked; dropping it is right and the hub has
            // already refused it at submit time in every case that matters.
            CommandKind::Answer { .. } => None,
        }
    }
}

/// One session: the engine, the transcript, the tools, the log and the store.
pub struct Harness<'a> {
    cfg: Config,
    engine: TurnEngine<'a>,
    session: Session,
    runtime: ToolRuntime,
    hub: Arc<Hub>,
    store: Option<Store>,
    transcript_id: String,
    /// How many ledger rows have reached the store.
    persisted: usize,
    system_updates: u64,
    /// The turn this harness last started. §4.5: a failure has to name one.
    last_turn_id: String,
    /// Read at open time from the backend, the gate and the seated schemas, so the
    /// adjudication disclosure is a reading rather than a claim. See [`GateWiring`].
    wiring: GateWiring,
    /// `Some` when this harness was rebuilt from the store rather than opened fresh.
    /// The daemon prints it; a head is told through the log, by the rows themselves.
    resumed: Option<ResumeReport>,
    /// A title this harness derived and the daemon has not yet published. See
    /// [`Harness::take_new_title`].
    new_title: Option<String>,
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
        Self::open_with(parts, cfg, hub, Box::new(NoBoundary), None)
    }

    /// As [`Harness::open`], with a gate and an extra tool.
    ///
    /// The gate is `NoBoundary` in M1 and consulted only for what is not a read, so
    /// no read-only tool has a code path to a question (clause 4). It is a
    /// parameter rather than a constant because W11 absorbs this seam, and a seam
    /// that only ever had one implementation is a seam nobody checked.
    pub fn open_with(
        parts: &'a Parts,
        mut cfg: Config,
        hub: Arc<Hub>,
        gate: Box<dyn Gate>,
        extra_tool: Option<Box<dyn Tool>>,
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
        }

        let backend = HostBackend::new(&cfg.workspace)
            .map_err(|e| HarnessError::Setup(format!("workspace {:?}: {e}", cfg.workspace)))?;

        // Retrieval is inert and stays inert: `ask_code` and `ask_corpus` abstain,
        // and nothing is behind them. T16.6 checked rather than recalled — no MCP
        // server is running on this box or on lubuntu3. A stub that answered would
        // be the exact failure §8.2 exists to prevent.
        let retrieval: Arc<dyn letibot_tools::builtins::retrieval::Retrieval> =
            Arc::new(letibot_tools::builtins::retrieval::Unavailable);
        let mut registry: Registry = letibot_tools::read_only_tools(retrieval)
            .map_err(|e| HarnessError::Setup(format!("registering the M1 tool set: {e}")))?;
        if let Some(t) = extra_tool {
            registry
                .register(t)
                .map_err(|e| HarnessError::Setup(format!("registering an extra tool: {e}")))?;
        }
        let registry = registry
            .resolve_role(&roles::m1_orchestrator())
            .map_err(|e| HarnessError::Setup(format!("seating the orchestrator role: {e}")))?;
        let schemas = registry.schemas();

        // Read, never asserted. `is_writable` is the backend's own answer, `describe`
        // is the gate's, and the write-tool question is answered by the schemas that
        // were actually seated a few lines above -- not by which role was requested.
        let wiring = GateWiring {
            adjudicator: gate.describe(),
            backend_writable: backend.is_writable(),
            has_write_tools: schemas
                .iter()
                .any(|s| s.access == letibot_tools::schema::Access::Write),
        };

        let spiller = build_spiller(&cfg)?;
        let runtime = ToolRuntime::new(registry, Box::new(backend))
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
            cfg.dialect.reasoning_field(),
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

        let (transcript_id, session, persisted, resume) = match resumed {
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
                if stored_sha != dialect_sha {
                    return Err(HarnessError::Setup(format!(
                        "session {} was recorded under dialect template {stored_sha} and \
                         this daemon renders {dialect_sha}. It was NOT resumed: the stored \
                         tokens came out of the other renderer, and appending this one's \
                         bytes to them would build a prompt no model was ever trained on \
                         — which the hash chain cannot catch, because every row is \
                         correctly hashed by whoever wrote it. Start the daemon with the \
                         dialect this session was recorded under.",
                        cfg.session_id
                    )));
                }

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

                let rows = session.ledger.rows().len();
                let report = ResumeReport {
                    transcript_id: transcript_id.clone(),
                    rows,
                    tokens: session.ledger.len(),
                    head: session.ledger_head(),
                    workspace: cfg.workspace.display().to_string(),
                    notes: std::mem::take(&mut notes),
                };
                (transcript_id, session, rows, Some(report))
            }
            None => {
                let transcript_id = format!("{}#t0", cfg.session_id);
                let session = engine
                    .open(&transcript_id, &prefix)
                    .map_err(|e| HarnessError::Setup(format!("opening the session: {e}")))?;
                if let Some(s) = &store {
                    let rec = StablePrefixRecord {
                        dialect_sha: dialect_sha.clone(),
                        system: prefix.system.clone(),
                        tools_json: prefix.tools_json.clone(),
                        tokens: session.ledger.prefix_tokens().to_vec(),
                        h_init: session.ledger.h_init(),
                        vocab_source: cfg.vocab_gguf.display().to_string(),
                    };
                    let prefix_id = s
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
                            approvers: vec![],
                        })
                        .map_err(|e| HarnessError::Store(e.to_string()))?;
                    }
                    if !transcript_exists(s, &transcript_id) {
                        s.put_transcript(&transcript_id, &cfg.session_id, &prefix_id)
                            .map_err(|e| HarnessError::Store(e.to_string()))?;
                    }
                }
                (transcript_id, session, 0, None)
            }
        };

        let h = Harness {
            wiring,
            cfg,
            engine,
            session,
            runtime,
            hub,
            store,
            transcript_id,
            persisted,
            system_updates: 0,
            last_turn_id: String::new(),
            resumed: resume,
            new_title: None,
        };
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

    /// What this session actually wired. The adjudication disclosure is computed
    /// from it; see [`GateWiring`] for why it is not a constant.
    pub fn wiring(&self) -> &GateWiring {
        &self.wiring
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn transcript_id(&self) -> &str {
        &self.transcript_id
    }

    pub fn items(&self) -> &[TranscriptItem] {
        &self.session.items
    }

    pub fn ledger_head(&self) -> String {
        self.session.ledger_head()
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
    pub fn submit(&mut self, text: &str) -> Result<Reply, HarnessError> {
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
        let item = self
            .cfg
            .dialect
            .wiring(self.cfg.effort.as_deref())
            .system_update(self.system_updates, text);
        self.submit_item(item)
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

    fn run_rounds(&mut self) -> Result<Reply, HarnessError> {
        let mut metrics = Vec::new();
        let mut tool_calls = 0usize;
        let mut truncated = false;

        for round in 0..self.cfg.max_tool_rounds {
            let mut sink = CapturingSink::new(self.hub.clone());
            let mut steering = HubSteering::new(self.hub.clone());
            let outcome = self
                .engine
                .run_turn_steered(&mut self.session, &mut sink, &mut steering);
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
                    Err(TurnFailure::EmptyLength { reason, .. }) => {
                        self.append_notice(&format!(
                            "Your previous turn hit the output token limit with nothing \
                             usable in it ({}). Nothing was recorded. Answer again, and \
                             put the answer before the reasoning if you are close to the \
                             limit.",
                            reason.as_str()
                        ))?;
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };

            let appended = ok.items.clone();
            self.reconcile(&mut sink, &appended);
            self.persist()?;
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
            metrics.push(ok.metrics);

            if calls.is_empty() {
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
            let mut tool_sink = ToolLogSink::new(self.hub.clone());
            let mut results = Vec::with_capacity(calls.len());
            for call in &calls {
                // Call order, and appended in call order. See point 3 above.
                let r = self.runtime.invoke(&turn_id, call, &mut tool_sink);
                results.push(ToolRuntime::transcript_item(&r));
            }
            let mut sink = CapturingSink::new(self.hub.clone());
            self.session
                .append_items(&self.engine, &results, &mut sink)?;
            self.reconcile(&mut sink, &results);
            self.persist()?;
        }
        Err(HarnessError::LoopBound {
            rounds: self.cfg.max_tool_rounds,
        })
    }

    fn append_notice(&mut self, text: &str) -> Result<(), HarnessError> {
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
    /// The database enforces the append-only property itself — a trigger refuses a
    /// `seq` that is not the next one and a `tok_offset` that does not continue the
    /// previous row — so a divergence between the ledger and the store is a failed
    /// INSERT rather than a quiet inconsistency.
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
