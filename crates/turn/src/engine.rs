//! The turn engine: render → tokenize → append → submit → stream → parse → append.
//!
//! §5.1 end to end, on §5.6's `/completion` fallback, which is what M1 runs on.
//!
//! # What a turn does to the token region
//!
//! ```text
//!   region after turn N        [ prefix | item | item | … | item ]
//!   submitted for turn N+1     [ prefix | item | item | … | item | <|assistant|><think> ]
//!                                                                  ^^^ uncommitted tail
//!   region after turn N+1      [ prefix | item | … | item | <|assistant|><think>…generated… ]
//! ```
//!
//! The generation prompt is submitted but **not** committed until the turn produces
//! something, because it is not a transcript item: folding it into a render would
//! break `render_incremental ≡ render` for any conversation whose next item is a
//! user message. If the turn fails, the tail was never committed, the region is
//! unchanged, and the next attempt re-sends exactly the same bytes. There is no
//! rollback path because there is nothing to roll back.
//!
//! # Generated tokens are committed as ids, never re-derived from text
//!
//! The ledger rows for a turn are cut out of the id array the server streamed. No
//! detokenize/retokenize round trip happens anywhere on this path, which is what
//! makes §18.1-I1 hold by construction rather than by assertion.
//!
//! # A failed turn cannot be recorded as a success
//!
//! `run_turn` returns `Result<TurnOk, TurnFailure>` and [`TurnOk`] has no
//! constructor that accepts a verdict which `may_record_as_success()` rejects. The
//! empty-content-plus-`length` case is therefore not *representable* as a completed
//! turn — which is the actual lesson of the nine unnoticed rows, rather than
//! "remember to check a flag".

use std::time::Instant;

use letibot_backend::BackendCaps;
use letibot_dialect::{
    ControlRole, DialectSpec, Parser, ReasoningField, RenderSpan, StablePrefix, TokenDecoder,
};
use letibot_tokencore::{
    ControlMap, TokenId, TokenLedger, Vocab, VocabDecoder, resolve, resolve_stops, tokenize_spans,
};
use letibot_transcript::TranscriptItem;
use serde_json::Value;

use crate::capture::FrameCapture;
use crate::completion::{Chunk, CompletionRequest, FinishReason};
use crate::events::{DeltaTarget, EventSink, TurnEvent, args_digest, head_hex};
use crate::guards::{GuardSet, Trip};
use crate::http::{self, Endpoint, Flow, HttpError};
use crate::items::{self};
use crate::length::{self, EmptyReason, LengthVerdict, SalvageBudget, TurnShape};
use crate::metrics::{TurnMetrics, cost_from};
use crate::prefix::{self, PrefixCheck, PrefixWitness};
use crate::renderer::PromptRenderer;
use crate::steering::{NoSteering, Pending, SteeringSource};
use crate::stream::{AbortCause, IdAccumulator, StreamError, StreamOutcome};

#[derive(Debug)]
pub enum EngineError {
    /// A control token or stop literal the vocabulary does not have as one entry.
    /// Raised at construction, which is the only moment it is cheap: a stop that is
    /// silently a multi-token sequence never fires, and the turn runs to `n_ctx`.
    Control(letibot_tokencore::ControlResolveError),
    Tokenize(String),
    Ledger(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Control(e) => write!(f, "{e}"),
            EngineError::Tokenize(m) | EngineError::Ledger(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// Why a turn did not produce a recordable result.
#[derive(Debug)]
pub enum TurnFailure {
    /// §5.7's two hard-fail cases. Never a success, under any policy value.
    EmptyLength {
        turn_id: String,
        reason: EmptyReason,
        metrics: Box<TurnMetrics>,
    },
    /// §5.7: any truncated argument fails the **whole** batch. The notices are what
    /// the model must be told, in tool results, not in a log.
    BatchTruncated {
        turn_id: String,
        truncated: Vec<String>,
        notices: Vec<String>,
        metrics: Box<TurnMetrics>,
    },
    /// R7, found live 2026-09-10: the turn stopped with the reasoning block still
    /// open and committed no assistant content — no visible text, no tool call.
    /// GLM's end-of-turn token is the *name* of a thing an agent may be asked to
    /// write, so the model can end its own turn mid-thought with a normal
    /// `stop`, and the conflation is in the vocabulary, not fixable at the
    /// sampler (`--logit-bias` measurably breaks the model's ordinary stop).
    /// What is fixable is the reporting: this is never an empty success. The
    /// loop's answer is steering, not a hard stop — see the harness arm.
    UnfinishedReasoning {
        turn_id: String,
        metrics: Box<TurnMetrics>,
    },
    /// The salvage cap is spent (§5.7's bounded retry).
    SalvageExhausted {
        turn_id: String,
        streak: u32,
    },
    /// §8.5's guard fired.
    Guard {
        turn_id: String,
        trip: Trip,
    },
    Http(HttpError),
    Stream(StreamError),
    Engine(EngineError),
}

impl std::fmt::Display for TurnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurnFailure::EmptyLength { reason, .. } => write!(
                f,
                "the turn hit the output limit having produced nothing usable ({}). \
                 This is a failed turn, not a short one.",
                reason.as_str()
            ),
            TurnFailure::BatchTruncated { truncated, .. } => write!(
                f,
                "the turn hit the output limit with {} truncated tool argument(s); the \
                 whole batch was refused",
                truncated.len()
            ),
            TurnFailure::UnfinishedReasoning { .. } => write!(
                f,
                "the turn stopped inside its own reasoning block and said nothing; \
                 this is a failed turn, not an empty one"
            ),
            TurnFailure::SalvageExhausted { streak, .. } => {
                write!(f, "{streak} consecutive length salvages; the cap is spent")
            }
            TurnFailure::Guard { trip, .. } => write!(f, "{}: {}", trip.code, trip.detail),
            TurnFailure::Http(e) => write!(f, "{e}"),
            TurnFailure::Stream(e) => write!(f, "{e}"),
            TurnFailure::Engine(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TurnFailure {}

impl From<HttpError> for TurnFailure {
    fn from(e: HttpError) -> Self {
        TurnFailure::Http(e)
    }
}

impl From<StreamError> for TurnFailure {
    fn from(e: StreamError) -> Self {
        TurnFailure::Stream(e)
    }
}

impl From<EngineError> for TurnFailure {
    fn from(e: EngineError) -> Self {
        TurnFailure::Engine(e)
    }
}

/// A turn that may be recorded.
///
/// There is no public constructor. [`TurnOk::new`] is private and asserts the
/// verdict, so the only way to obtain one is to have passed §5.7.
#[derive(Debug, Clone)]
pub struct TurnOk {
    pub turn_id: String,
    pub items: Vec<TranscriptItem>,
    pub metrics: TurnMetrics,
    pub verdict: LengthVerdict,
    /// The output was cut short but is usable. Surfaced, never swallowed: §5.7's
    /// "a truncated turn is never reported as a completed one" is this field plus
    /// the `TurnFinished` event's `finish_reason`.
    pub truncated: bool,
    /// Steering messages injected at this turn's step boundary (§5.8).
    pub steering_applied: Vec<TranscriptItem>,
    /// **Steering that was already waiting when this generation began**, appended
    /// before the prompt was built rather than after the model answered.
    ///
    /// Kept apart from [`Self::steering_applied`] because the two groups straddle
    /// the model's own rows in the transcript, and the caller reconciles ledger
    /// ids positionally: before, then the model's items, then after. One vector
    /// could not say which side a message landed on.
    pub steering_before: Vec<TranscriptItem>,
}

impl TurnOk {
    fn new(
        turn_id: String,
        items: Vec<TranscriptItem>,
        metrics: TurnMetrics,
        verdict: LengthVerdict,
        interrupted: bool,
        steering_applied: Vec<TranscriptItem>,
        steering_before: Vec<TranscriptItem>,
    ) -> TurnOk {
        assert!(
            verdict.may_record_as_success(),
            "BUG: a verdict of {verdict:?} reached TurnOk. §5.7 says this turn failed; \
             constructing it as a success is the defect this type exists to prevent."
        );
        TurnOk {
            // Two different ways to be cut short and both must show. §5.7's
            // `length` is one; an urgent steering message that stopped generation
            // at the next token is the other, and its partial output *is* kept
            // (§5.8, and §13.2's cache reason). A kept partial reported as a
            // complete turn is the same defect as a swallowed `finish_reason`,
            // arriving through the other door.
            truncated: interrupted || matches!(verdict, LengthVerdict::TruncatedText),
            turn_id,
            items,
            metrics,
            verdict,
            steering_applied,
            steering_before,
        }
    }
}

/// Everything about the model that does not change between turns.
pub struct TurnEngine<'a> {
    vocab: &'a Vocab,
    control: ControlMap,
    stop_ids: Vec<TokenId>,
    client_stop_ids: Vec<TokenId>,
    renderer: &'a dyn PromptRenderer,
    parser: &'a dyn Parser,
    spec: DialectSpec,
    pub endpoint: Endpoint,
    pub caps: BackendCaps,
    pub model: String,
    pub sampling: Value,
    pub salvage: SalvageBudget,
    /// **The server's own media marker**, from `/props` at startup, or `None` for an endpoint that
    /// takes no images — a metered provider, or a local server without `mtmd`.
    ///
    /// `None` is a real answer and not a failure, and it is the honest one for most endpoints: a
    /// prompt carrying an image is then sent **without** the image, and the payload's own sentence
    /// says so (*"the image is attached to this result"*). That is a picture the model was told about
    /// and not given, which is why `delivered` exists on the row to make the difference visible.
    ///
    /// Never a dialect's vision tokens: the server answers those with `Failed to tokenize prompt`
    /// (measured — see `serving::served_media_marker`).
    pub media_marker: Option<String>,
    /// Where a refused frame and its neighbours are written (T23). On by default;
    /// see [`FrameCapture`] for why the default is on rather than off.
    pub frame_capture: FrameCapture,
    /// **The whole turn's start in Unix ms, if the caller knows it.**
    ///
    /// See [`TurnEvent::TurnStarted::began_ms`]. The engine runs ONE round — `run_turn_steered`
    /// is called inside the daemon's round loop — so the engine cannot know when the turn
    /// began. Whoever runs the rounds stamps this, and `None` means nobody measured it, which
    /// a head draws as *started before this head attached* rather than a duration nobody took.
    pub turn_began_ms: Option<u64>,
    /// **Hand the next turn a lead with reasoning already closed.**
    ///
    /// False for every turn but the summary. Not a config field and not a
    /// constructor argument: it is a property of ONE turn, set around it by
    /// [`crate::compaction::run_compaction`] and cleared however that turn ends.
    /// A session-lifetime switch here would be a way to turn a model's reasoning
    /// off by accident and never notice.
    suppress_reasoning: bool,
}

impl TurnEngine<'_> {
    /// Run `f` with the next turn's lead closing the reasoning block.
    ///
    /// Scoped rather than a pair of setters, so the flag cannot outlive the turn
    /// it was set for — including when that turn fails, which for a summary is the
    /// common case and exactly when a leaked flag would be hardest to see.
    pub fn without_reasoning<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let was = std::mem::replace(&mut self.suppress_reasoning, true);
        let out = f(self);
        self.suppress_reasoning = was;
        out
    }
}

/// Whether diagnostic-only verdicts are wanted this run: `LETIBOT_DEBUG=1`.
///
/// An environment variable rather than a config field on purpose — this decides
/// what is PRINTED, not what the harness does, and a session should not have to
/// be restarted to see one more line. Read once.
fn debug_warnings() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            std::env::var("LETIBOT_DEBUG").ok().as_deref(),
            Some("1") | Some("true")
        )
    })
}

impl<'a> TurnEngine<'a> {
    /// Resolve the dialect against the vocabulary and fail **now** if it does not
    /// fit. Every failure this can raise is one that is otherwise silent at
    /// runtime.
    ///
    /// Seven arguments, and a builder would be worse: every one of them is a fact
    /// the engine cannot invent, and a builder's `Option` per field is an
    /// invitation to leave one out. The clippy lint is about ergonomics; the
    /// alternative here trades an awkward call site for a silently mis-wired one.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vocab: &'a Vocab,
        renderer: &'a dyn PromptRenderer,
        parser: &'a dyn Parser,
        endpoint: Endpoint,
        caps: BackendCaps,
        model: impl Into<String>,
        sampling: Value,
    ) -> Result<Self, EngineError> {
        let spec = renderer.spec().clone();
        let control = resolve(vocab, &spec.control_tokens).map_err(EngineError::Control)?;
        let stop_ids = resolve_stops(vocab, &spec.stop_tokens).map_err(EngineError::Control)?;
        // A stop that the vocabulary marks end-of-generation is one the *server*
        // stops on. Enforcing it here as well would close the socket one frame
        // early and throw away the terminal frame — with it the `timings`, so a
        // turn that reused its entire history would report `cache_n = 0` and every
        // post-flight check would see a cold prefill that never happened.
        //
        // A stop that is **not** EOG is the case §7.1 warns about: the server has
        // no reason to halt on it, so without this the turn runs to `n_ctx`. That
        // is the half worth enforcing, and it is the half that is left.
        let client_stop_ids = stop_ids
            .iter()
            .copied()
            .filter(|id| !vocab.is_eog(*id))
            .collect();
        Ok(TurnEngine {
            vocab,
            control,
            stop_ids,
            client_stop_ids,
            renderer,
            parser,
            spec,
            endpoint,
            caps,
            model: model.into(),
            sampling,
            salvage: SalvageBudget::default(),
            // Set by whoever knows the endpoint's `/props`; `None` means no images. See the field.
            media_marker: None,
            frame_capture: FrameCapture::default(),
            // Every turn reasons unless one asks not to; see `without_reasoning`.
            suppress_reasoning: false,
            turn_began_ms: None,
        })
    }

    pub fn spec(&self) -> &DialectSpec {
        &self.spec
    }

    pub fn control(&self) -> &ControlMap {
        &self.control
    }

    pub fn stop_ids(&self) -> &[TokenId] {
        &self.stop_ids
    }

    /// The subset this engine halts on itself, because the server will not.
    pub fn client_stop_ids(&self) -> &[TokenId] {
        &self.client_stop_ids
    }

    /// **The prompt as the text a server tokenizes itself**, with `marker` where each image sits.
    ///
    /// `None` when the prompt places no image at all, which is every turn but the few that read one.
    ///
    /// # Why this walks the ids rather than re-rendering
    ///
    /// The ledger's token region *is* the render, tokenized. Re-rendering would be a second walk of
    /// the same fact — the shape of every defect this tree has had today — and the two could drift
    /// the moment a renderer changed. So this reads the very ids about to be submitted and replaces
    /// the three image tokens with the server's single marker, which cannot describe a different
    /// prompt from the one the ids describe because it is built out of them.
    ///
    /// MEASURED, and the reason the string form is affordable at all (`docs/leticode.md`): a prompt
    /// framed by control tokens tokenizes identically as text and as ids, so the server's
    /// tokenization and the ledger's agree token for token.
    ///
    /// The return is the string and **how many markers it placed**, so a caller can hold that
    /// against the attachments it has and refuse to send pictures it cannot place.
    fn prompt_as_text(&self, tokens: &[TokenId], marker: &str) -> Option<(String, usize)> {
        let decoder = self.decoder();
        let mut out = String::new();
        let mut run: Vec<TokenId> = Vec::new();
        let mut images = 0usize;
        let mut flush = |run: &mut Vec<TokenId>, out: &mut String| {
            if !run.is_empty() {
                out.push_str(&decoder.decode(run));
                run.clear();
            }
        };
        for &id in tokens {
            match decoder.control_role(id) {
                // One image is ONE marker: the server emits the three tokens itself once it has the
                // picture, so the opening role places the marker and the other two are swallowed.
                Some(ControlRole::ImageOpen) => {
                    flush(&mut run, &mut out);
                    out.push_str(marker);
                    images += 1;
                }
                Some(ControlRole::Image) | Some(ControlRole::ImageClose) => {
                    flush(&mut run, &mut out);
                }
                _ => run.push(id),
            }
        }
        flush(&mut run, &mut out);
        (images > 0).then_some((out, images))
    }

    fn decoder(&self) -> VocabDecoder<'_> {
        VocabDecoder::new(self.vocab, &self.control)
    }

    /// The literal a control id spells, so a live stream can cut it out of the text
    /// the server sends alongside it.
    ///
    /// By id through the dialect's own table, never by scanning the text for
    /// something that looks like a boundary — that is the rule [`ControlRole`] and
    /// [`RenderSpan`] exist to enforce in both directions. A linear walk over ten
    /// entries, reached only on a token that is already known to be a boundary.
    ///
    /// Note that a control token whose literal the server does **not** put in
    /// `content` (a `CONTROL`-attribute token, as against Qwen's `USER_DEFINED`
    /// `</think>`) simply is not found in the chunk, and the channel switch happens
    /// with nothing to cut. Both cases are correct and neither needs to be
    /// distinguished here.
    fn control_literal(&self, id: TokenId) -> Option<&str> {
        self.spec
            .control_tokens
            .iter()
            .find(|c| self.control.id(&c.literal) == Some(id))
            .map(|c| c.literal.as_ref())
    }

    fn tokenize(&self, spans: &[RenderSpan]) -> Result<Vec<TokenId>, EngineError> {
        tokenize_spans(self.vocab, &self.control, spans)
            .map_err(|e| EngineError::Tokenize(e.to_string()))
    }

    /// Open a session over a stable prefix.
    pub fn open(&self, transcript_id: &str, prefix: &StablePrefix) -> Result<Session, EngineError> {
        let spans = self.renderer.render(prefix, &[]);
        let tokens = self.tokenize(&spans)?;
        let ledger = TokenLedger::new(transcript_id, &tokens)
            .map_err(|e| EngineError::Ledger(e.to_string()))?;
        Ok(Session {
            transcript_id: transcript_id.to_string(),
            ledger,
            items: Vec::new(),
            witness: None,
            turn_seq: 0,
        })
    }
}

/// One transcript, its token region, and what the last turn left for the next one
/// to check.
pub struct Session {
    pub transcript_id: String,
    pub ledger: TokenLedger,
    pub items: Vec<TranscriptItem>,
    witness: Option<PrefixWitness>,
    turn_seq: u64,
}

impl Session {
    /// Seat a session over a ledger that was rebuilt from the store.
    ///
    /// `pub(crate)` and not public: the only correct way in is
    /// [`crate::resume::restore_parts`], which has verified the chain twice before
    /// it gets here. A public constructor taking a ledger would let a caller seat a
    /// session over a region nothing checked, which is the one thing the whole
    /// resume path exists to make impossible.
    ///
    /// `witness` is `None` on purpose. A [`PrefixWitness`] records what the *server*
    /// held after the last turn, and this process has not run one — the slot may
    /// have been evicted, refilled by another session, or belong to a server that
    /// has since restarted. So the first turn after a resume reports no `f_keep`
    /// rather than a number computed against a cache nobody looked at. See
    /// `crate::prefix`.
    ///
    /// `turn_seq` starts at the item count. See [`crate::resume`]'s header: it is a
    /// watermark that keeps turn ids unique across a restart, not a count of turns.
    pub(crate) fn from_restored(
        transcript_id: String,
        ledger: TokenLedger,
        items: Vec<TranscriptItem>,
    ) -> Session {
        let turn_seq = items.len() as u64;
        Session {
            transcript_id,
            ledger,
            items,
            witness: None,
            turn_seq,
        }
    }

    /// Append items rendered from the transcript — user turns, tool results,
    /// system updates, steering messages.
    ///
    /// One ledger row per item. Each item is rendered against the history as it
    /// stood before it, which is what `render_incremental`'s contract requires and
    /// what keeps the rows aligned with the items.
    pub fn append_items(
        &mut self,
        engine: &TurnEngine<'_>,
        new_items: &[TranscriptItem],
        sink: &mut dyn EventSink,
    ) -> Result<(), EngineError> {
        for item in new_items {
            let spans = engine
                .renderer
                .render_incremental(&self.items, std::slice::from_ref(item));
            let tokens = engine.tokenize(&spans)?;
            let item_id = format!("{}.{}", self.transcript_id, self.items.len());
            let row = self
                .ledger
                .append(&item_id, &tokens)
                .map_err(|e| EngineError::Ledger(e.to_string()))?;
            let kind = letibot_tokencore::store::item_kind(item);
            let head = head_hex(&row.h_k);
            let n = row.tok_len;
            self.items.push(item.clone());
            sink.emit(TurnEvent::TranscriptAppended {
                item_id,
                kind,
                ledger_head: head,
                tokens: n,
            });
        }
        Ok(())
    }

    pub fn ledger_head(&self) -> String {
        head_hex(&self.ledger.head())
    }

    /// What the last turn recorded for the prefix check. `None` before the first.
    pub fn witness(&self) -> Option<&PrefixWitness> {
        self.witness.as_ref()
    }
}

impl TurnEngine<'_> {
    /// Run one turn with no steering source.
    pub fn run_turn(
        &mut self,
        session: &mut Session,
        sink: &mut dyn EventSink,
    ) -> Result<TurnOk, TurnFailure> {
        self.run_turn_steered(session, sink, &mut NoSteering)
    }

    /// Run one turn, injecting steering at the step boundary (§5.8).
    pub fn run_turn_steered(
        &mut self,
        session: &mut Session,
        sink: &mut dyn EventSink,
        steering: &mut dyn SteeringSource,
    ) -> Result<TurnOk, TurnFailure> {
        session.turn_seq += 1;
        let turn_id = format!("{}#{}", session.transcript_id, session.turn_seq);
        let started = Instant::now();

        sink.emit(TurnEvent::TurnStarted {
            turn_id: turn_id.clone(),
            model: self.model.clone(),
            ledger_head: session.ledger_head(),
            // The engine sees ONE round, so it cannot know when the turn began; the emitter
            // stamps it. See `began_ms`. (`None` here is the honest answer from a caller that
            // has nothing to stamp with — a test, or a round with no prompt above it.)
            began_ms: self.turn_began_ms,
        });

        // Shared with the stream loop: a non-urgent message that arrives mid-turn
        // is absorbed there and injected here. Two `Pending`s would mean the queue
        // that saw the message is not the queue the step boundary drains, and the
        // correction would be silently dropped — which is worse than not
        // implementing steering at all.
        let mut pending = Pending::new();
        // **Take the operator's words before spending a prompt, not after.**
        //
        // The poll used to live only inside the stream loop and at the step
        // boundary under it, so the window where nobody listened was exactly "a
        // tool is running" — and it cost a round on top. Typing during a
        // three-minute `job_wait` left the message in the hub queue; the next
        // round then built and sent its whole prompt without it, generated deaf,
        // and only appended it afterwards. The operator, watching their line sit
        // there through all of that: *"i backgrounded a job but my messsage still
        // queued"*, and then the rule — *"basically it should be a little bit
        // greedy with my messages"*.
        //
        // So: greedy. Anything waiting when a generation begins goes in FIRST, and
        // the model reads it in this prompt rather than the next one. It also puts
        // the row in its natural place — after the tool results of the round
        // before, instead of wedged between an assistant's tool calls and their
        // results, which is where the end-of-generation append leaves it.
        // An urgent message is not queued by `absorb`; it is returned to be acted
        // on, and there is no generation here to act on yet. Held, so the stream
        // loop's first poll finds it — dropping it would turn an interrupt into
        // silence.
        if let Some(u) = pending.absorb(steering) {
            pending.hold_urgent(u);
        }
        let steering_before = pending.take_items();
        if !steering_before.is_empty() {
            session.append_items(self, &steering_before, sink)?;
        }
        // **Which lead this turn gets**, and the only turn that gets the other one
        // is the summary. See `PromptRenderer::generation_prompt_closing_reasoning`:
        // a summary turn runs when the window is nearly full, and a lead that opens
        // `<think>` spends the little that is left before it writes a word.
        let lead = self.tokenize(&if self.suppress_reasoning {
            self.renderer.generation_prompt_closing_reasoning()
        } else {
            self.renderer.generation_prompt()
        })?;
        let mut prompt = session.ledger.tokens().to_vec();
        prompt.extend_from_slice(&lead);
        let prompt_tokens = prompt.len() as u64;

        // Which channel the model starts on. Read off the dialect's own generation
        // prompt — the tokens we are about to submit — rather than assumed, and by
        // the same function `items::produce` uses to decide the same thing about
        // the same tokens. Seeding this from a *generated* `ThinkOpen` was one half
        // of T12: the lead already opened the block, so the model reasons from
        // token one while every head is told it is assistant text.
        let opens_in_reasoning = items::lead_opens_reasoning(&lead, &self.decoder());
        // **And the same prompt as TEXT, when it carries a picture.** Built from the ids that are
        // about to be submitted rather than from a second render, so the two cannot describe two
        // different prompts — and the media is taken in item order, which is the order the markers
        // appear, because the server substitutes the pictures at the markers in sequence.
        let multimodal = self.media_marker.as_deref().and_then(|marker| {
            let (string, images) = self.prompt_as_text(&prompt, marker)?;
            let media = letibot_transcript::media::media_in_order(&session.items);
            if images != media.len() {
                // **Fail safe, and loudly.** A mismatch means the renderer placed a different number
                // of images than the transcript holds — the one failure that would substitute a
                // picture into the wrong place, which is worse than substituting none. The ids path
                // is taken instead: the model is told about an image and not given it, which the
                // row's own sentence already says, and the `delivered` report is where a reader
                // sees it. The line below is the only trace, and it is meant to be read.
                eprintln!(
                    "turn: {images} image marker(s) against {} attachment(s) — sending no images \
                     rather than the wrong ones",
                    media.len()
                );
                return None;
            }
            // **And the row is told the bytes went out.** One place knows this — here — because
            // here is where the request is built. See `Media::delivered` for the argument; the short
            // of it is that a picture the daemon dropped and one the model ignored read identically
            // on a transcript that does not say which, and only one of those is the operator's to
            // fix.
            letibot_transcript::media::mark_delivered(&mut session.items);
            Some((string, media))
        });
        let (outcome, guard_trip) = self.stream_turn(
            &turn_id,
            prompt.clone(),
            multimodal,
            opens_in_reasoning,
            sink,
            steering,
            &mut pending,
        )?;
        let wall_ms = started.elapsed().as_millis() as u64;

        if let Some(trip) = guard_trip {
            sink.emit(TurnEvent::Warning {
                code: trip.code,
                detail: trip.detail.clone(),
            });
            sink.emit(TurnEvent::TurnInterrupted {
                turn_id: turn_id.clone(),
                reason: trip.code.to_string(),
                // Nothing was committed: the generation-prompt tail was never part
                // of the region, so the transcript is exactly where it was.
                partial_kept: false,
            });
            sink.emit(TurnEvent::TurnFinished {
                turn_id: turn_id.clone(),
                finish_reason: FinishReason::Aborted,
                metrics: Box::new(
                    self.metrics_for(
                        &turn_id,
                        session,
                        &outcome,
                        prompt_tokens,
                        wall_ms,
                        FinishReason::Aborted,
                        PrefixCheck::Skipped {
                            reason: "SKIPPED I1: the turn was aborted by a guard, so no items \
                                 were committed and there is no new prefix to check. This \
                                 did not run and it is not a pass."
                                .into(),
                        },
                    ),
                ),
            });
            return Err(TurnFailure::Guard { turn_id, trip });
        }

        let decoder = self.decoder();
        let reasoning_field = match self.spec.reasoning_field {
            ReasoningField::ReasoningContent => {
                letibot_transcript::ReasoningField::ReasoningContent
            }
            ReasoningField::Inline => letibot_transcript::ReasoningField::Inline,
        };
        let mut produced = items::produce(
            &lead,
            &outcome.ids,
            &self.stop_ids,
            self.parser,
            &decoder,
            reasoning_field,
        );
        if let Err(gap) = items::rows_cover_every_token(&produced) {
            // Not recoverable by guessing: a token with no row is a token the next
            // prompt will re-send, and the cache diverges on it.
            sink.emit(TurnEvent::Warning {
                code: "row_coverage_gap",
                detail: gap.clone(),
            });
            return Err(TurnFailure::Stream(StreamError::Protocol(format!(
                "the turn's tokens do not tile its items: {gap}"
            ))));
        }

        let calls = produced.tool_calls();
        for call in &calls {
            sink.emit(TurnEvent::ToolCallProposed {
                turn_id: turn_id.clone(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                args_digest: args_digest(&call.arguments),
                // §4.1. The digest is a correlation key and is useless on a
                // screen; these are what a person reads while the call runs. They
                // go no further than the in-process sink — the lift cuts them to a
                // bounded label before anything reaches a socket.
                arguments: call.arguments.clone(),
            });
        }

        let hit_length = outcome.final_chunk.finish_reason == FinishReason::Length;
        let verdict = length::classify(
            hit_length,
            TurnShape {
                visible_text: &produced.visible_text,
                reasoning_text: &produced.reasoning_text,
                tool_calls: &calls,
            },
        );

        if !verdict.may_record_as_success() {
            // Nothing is committed. §5.7 is explicit that this is a failed turn,
            // and the region must not carry a turn that failed.
            let metrics = self.metrics_for(
                &turn_id,
                session,
                &outcome,
                prompt_tokens,
                wall_ms,
                outcome.final_chunk.finish_reason,
                PrefixCheck::Skipped {
                    reason: "SKIPPED I1: the turn failed under §5.7 and committed no items, \
                             so there is no new prefix to check. This did not run and it is \
                             not a pass."
                        .into(),
                },
            );
            sink.emit(TurnEvent::TurnFinished {
                turn_id: turn_id.clone(),
                finish_reason: outcome.final_chunk.finish_reason,
                metrics: Box::new(metrics.clone()),
            });
            return Err(match verdict {
                LengthVerdict::HardFail(reason) => {
                    sink.emit(TurnEvent::Warning {
                        code: "length_empty_turn",
                        detail: format!(
                            "finish_reason=length with {} — the turn produced nothing \
                             recordable and is failed, not shortened",
                            reason.as_str()
                        ),
                    });
                    if !self.salvage.salvaged() {
                        return Err(TurnFailure::SalvageExhausted {
                            turn_id,
                            streak: self.salvage.streak(),
                        });
                    }
                    TurnFailure::EmptyLength {
                        turn_id,
                        reason,
                        metrics: Box::new(metrics),
                    }
                }
                LengthVerdict::ToolCallsTruncated { truncated } => {
                    let notices = calls
                        .iter()
                        .map(|c| length::batch_failed_notice(&c.name))
                        .collect();
                    sink.emit(TurnEvent::Warning {
                        code: "length_batch_refused",
                        detail: format!(
                            "{} truncated argument(s); none of the {} call(s) were executed",
                            truncated.len(),
                            calls.len()
                        ),
                    });
                    if !self.salvage.salvaged() {
                        return Err(TurnFailure::SalvageExhausted {
                            turn_id,
                            streak: self.salvage.streak(),
                        });
                    }
                    TurnFailure::BatchTruncated {
                        turn_id,
                        truncated,
                        notices,
                        metrics: Box::new(metrics),
                    }
                }
                other => unreachable!("{other:?} may be recorded as a success"),
            });
        }

        // R7's turn-boundary check. The length gate above only speaks when
        // `finish_reason == length`; on a normal `stop` the classifier has no
        // opinion, and this is exactly the hole the live finding fell through:
        // GLM ends its own turn mid-thought with a normal stop, the parse holds
        // an unterminated reasoning block and nothing else, and the turn used to
        // commit as an ordinary empty one. The §5.4 fallback item (an empty
        // `Assistant` emitted so tokens tile) does not count as content — it
        // exists for coverage, not for saying something. An aborted outcome is
        // not this failure: §5.8's kept partial and the steering interrupt have
        // their own reporting, and folding an interrupted mid-reasoning stream
        // into `UnfinishedReasoning` would misname a turn we deliberately kept.
        let has_content =
            !produced.visible_text.trim().is_empty() || !produced.tool_calls().is_empty();
        if outcome.aborted.is_none() && produced.ended_in_reasoning && !has_content {
            let metrics = self.metrics_for(
                &turn_id,
                session,
                &outcome,
                prompt_tokens,
                wall_ms,
                outcome.final_chunk.finish_reason,
                PrefixCheck::Skipped {
                    reason: "SKIPPED I1: the turn ended inside a reasoning block with no \
                             assistant content and committed no items, so there is no new \
                             prefix to check. This did not run and it is not a pass."
                        .into(),
                },
            );
            sink.emit(TurnEvent::Warning {
                code: "ended_in_reasoning",
                detail: format!(
                    "the turn stopped with the reasoning block still open and {} chars \
                     of unterminated reasoning but no visible text and no tool call; \
                     failed as UnfinishedReasoning, not recorded as success",
                    produced.reasoning_text.len()
                ),
            });
            sink.emit(TurnEvent::TurnFinished {
                turn_id: turn_id.clone(),
                finish_reason: outcome.final_chunk.finish_reason,
                metrics: Box::new(metrics.clone()),
            });
            if !self.salvage.salvaged() {
                return Err(TurnFailure::SalvageExhausted {
                    turn_id,
                    streak: self.salvage.streak(),
                });
            }
            return Err(TurnFailure::UnfinishedReasoning {
                turn_id,
                metrics: Box::new(metrics),
            });
        }
        self.salvage.cleared();

        // §5.8's escape hatch fired: generation stopped at the next token and the
        // partial output was kept. Announced as an interruption, because a kept
        // partial that reports as a complete turn is §5.7's failure with a
        // different cause.
        //
        // T23 arrives through the same door and for the same reason. A frame that
        // did not account for itself is still refused — the ids after it never
        // reach the ledger — but the ids before it were each checked against the
        // server's own counter and are exactly as trustworthy as they were a frame
        // earlier. Failing the whole turn over the tail threw away an answer the
        // operator was waiting on and left `nothing was recorded` behind; keeping
        // the head and marking the seam is the same trade §5.8 already makes.
        //
        // **The refusal is one; the reason it is refused is two.** `stream::Mismatch` reads
        // the direction of the disagreement, and the sentence below says which — the
        // server's build when ids were withheld, the captured frames when more arrived than
        // were counted. Leaving the sentence as three bare numbers was R12's defect in a
        // second family: a reader learned that something disagreed and not what to do.
        let interrupt_reason = match &outcome.aborted {
            Some(AbortCause::Steering(_)) => Some("steering_urgent".to_string()),
            // **Two faults, one prefix, and the reading is what the operator acts on.**
            // `frame_mismatch` stays the prefix so a reader — and a grep of a log — sees
            // one kind of event; the clause after it says which of the two, because the
            // first thing to check differs: the server's build when ids were WITHHELD,
            // the captured frames when more were sent than counted. See
            // [`crate::stream::Mismatch`], which is where the line between them is drawn
            // once.
            Some(AbortCause::FrameMismatch {
                n_decoded,
                previous,
                ids,
                reading,
            }) => Some(match reading {
                // **The word `withheld` is load-bearing**: it is the reading's name, it is
                // what an operator greps a session log for, and the other arm deliberately
                // does NOT contain it — so a hit is the UTF-8 gate and nothing else.
                crate::stream::Mismatch::Withheld => format!(
                    "frame_mismatch: the server withheld {} of the {} token(s) it counted \
                     (tokens_predicted {previous} -> {n_decoded}, {ids} id(s) sent) — the \
                     shape of a token whose bytes end mid-character, which is llama.cpp \
                     before `d10f94713` (branch `glm-all`). Check the server's build; the \
                     frames are in the capture below.",
                    n_decoded - previous - *ids as u64,
                    n_decoded - previous
                ),
                crate::stream::Mismatch::OverSent => format!(
                    "frame_mismatch: the server sent {ids} id(s) for {} counted \
                     (tokens_predicted {previous} -> {n_decoded}) — MORE than it counted, \
                     which suppression cannot explain (it only ever removes ids), so this is \
                     not the UTF-8 gate. Read the frames in the capture below.",
                    n_decoded - previous
                ),
            }),
            _ => None,
        };
        // The same two ways `TurnOk::truncated` counts — §5.7's kept text and
        // §5.8's kept partial. Stamped onto the rows before they are committed so
        // the transcript and the turn record answer the question identically.
        produced.mark_truncated(
            interrupt_reason.is_some() || matches!(verdict, LengthVerdict::TruncatedText),
        );

        // Commit. The rows are cut out of the id array the server streamed; nothing
        // is re-rendered and nothing is re-tokenized — with **one** exception,
        // below, and it is the only place in this engine where the ledger
        // deliberately stops being byte-identical to what the model produced.
        let mut appended = Vec::new();
        for (n, produced_item) in produced.items.iter().enumerate() {
            let item_id = format!("{turn_id}.{n}");

            // **An abandoned thought is committed as the sentence that replaces
            // it, not as the thought.**
            //
            // The next turn's prompt is `session.ledger.tokens()` and nothing
            // else — items are never re-rendered on the ordinary path. So a
            // renderer that elides a stopped reasoning block does nothing for the
            // very next turn, which was the defect in the first attempt at this:
            // the row was marked correctly and the mark was never reached.
            //
            // Measured on the operator's own session, 2026-09-18: a model began
            // counting parentheses by hand, produced 25464 tokens of `+ 0 + 0 +
            // 0`, and was stopped. Every later turn replayed those tokens from
            // the ledger — and the model, reading its own abandoned loop as
            // history, went back to counting by hand. "it counts them again by
            // hand lol".
            //
            // WHY THIS DOES NOT BREAK §18.1-I1. The generation-inclusive prefix
            // invariant exists so that a CONTINUING generation is never
            // re-tokenized: tokens the model produced and will keep building on
            // must be replayed exactly, because detokenize/retokenize does not
            // round-trip. An aborted draft is not continuing. It was stopped on
            // purpose, by a person, and the next turn begins a new generation
            // whose prefix this row is merely history. Handing it back verbatim
            // is what made the model resume it.
            //
            // What is NOT lost: `item_json` keeps the whole text, so the store,
            // a resume, `/gate` and any later reader still see every token the
            // model produced. The split is the one the dialect re-render already
            // relies on — the item is the record, the ledger is what gets
            // replayed — and here they are deliberately different.
            //
            // The cost is a re-prefill of the tail from this row on. That is what
            // was happening anyway, over 25464 tokens; it now happens over about
            // forty.
            let elided;
            let ids: &[TokenId] = if matches!(
                &produced_item.item,
                TranscriptItem::Reasoning {
                    truncated: true,
                    ..
                }
            ) {
                // Rendered against the history as it stands before this item,
                // which is `render_incremental`'s contract and the same one
                // `Session::append_items` renders under.
                let spans = self
                    .renderer
                    .render_incremental(&session.items, std::slice::from_ref(&produced_item.item));
                elided = self.tokenize(&spans)?;
                &elided
            } else {
                &produced.tokens[produced_item.range.clone()]
            };
            let row = session
                .ledger
                .append(&item_id, ids)
                .map_err(|e| TurnFailure::Engine(EngineError::Ledger(e.to_string())))?;
            let head = head_hex(&row.h_k);
            let tokens = row.tok_len;
            let kind = letibot_tokencore::store::item_kind(&produced_item.item);
            session.items.push(produced_item.item.clone());
            appended.push(produced_item.item.clone());
            sink.emit(TurnEvent::TranscriptAppended {
                item_id,
                kind,
                ledger_head: head,
                tokens,
            });
        }

        // Post-flight, against the turn that came before this one. The check is
        // over the **submitted prompt**, which is the thing the invariant is about,
        // rather than over the region it was built from.
        let check = prefix::check(
            &self.caps,
            session.witness.as_ref(),
            &prompt,
            outcome.final_chunk.timings.cache_n,
        );
        if let Some((code, detail)) = check.warning() {
            // A verdict the operator cannot act on goes to the debug channel, not
            // to their screen — see `PrefixCheck::diagnostic_only`. The
            // measurement still happens and the numbers are still carried; what
            // changes is whether a line nobody can do anything about competes for
            // attention with `prefix_divergence`, which is our defect, and
            // `prefix_check_skipped`, which means nothing was checked.
            if !check.diagnostic_only() || debug_warnings() {
                sink.emit(TurnEvent::Warning { code, detail });
            }
        }
        // Independently, §4.3 clause 5: the chain must still agree with the tokens.
        // Cheap next to a prefill, and it is what turns "append-only by
        // construction" back into a measurement.
        if let Err(e) = session.ledger.verify_chain() {
            sink.emit(TurnEvent::Warning {
                code: "ledger_chain_mismatch",
                detail: e.to_string(),
            });
        }

        session.witness = Some(PrefixWitness::record(
            &prompt,
            &produced.tokens[lead.len()..],
        ));

        let metrics = self.metrics_for(
            &turn_id,
            session,
            &outcome,
            prompt_tokens,
            wall_ms,
            outcome.final_chunk.finish_reason,
            check,
        );

        let interrupted = interrupt_reason.is_some();
        if let Some(reason) = interrupt_reason {
            sink.emit(TurnEvent::TurnInterrupted {
                turn_id: turn_id.clone(),
                reason,
                partial_kept: !appended.is_empty(),
            });
        }

        sink.emit(TurnEvent::TurnFinished {
            turn_id: turn_id.clone(),
            finish_reason: outcome.final_chunk.finish_reason,
            metrics: Box::new(metrics.clone()),
        });

        // §5.8's step boundary: the generation is complete, so anything that
        // arrived during it — whether the stream loop saw it or it landed after —
        // goes in now, as a plain user append.
        // **A HELD URGENT IS HELD, NOT DROPPED — the same two lines the greedy poll above uses.**
        // `absorb` returns an urgent rather than queueing it, and this site ignored the return, so an
        // interrupt that arrived when there was no generation to stop was DISCARDED here. `Pending`
        // says what that costs: *"Dropping it would turn an interrupt into silence, which is the
        // worst thing this type could do."* MEASURED as silence, on the operator's head: four presses
        // of `esc esc`, four `interrupt_idle` warnings, and no interrupt ever reached the engine.
        if let Some(u) = pending.absorb(steering) {
            pending.hold_urgent(u);
        }
        let steering_items = pending.take_items();
        if !steering_items.is_empty() {
            session.append_items(self, &steering_items, sink)?;
        }

        Ok(TurnOk::new(
            turn_id,
            appended,
            metrics,
            verdict,
            interrupted,
            steering_items,
            steering_before,
        ))
    }

    /// Submit and consume the stream. Returns the outcome and any guard trip.
    ///
    /// # The channel a `Delta` announces is decided by ids, before any text moves
    ///
    /// `opens_in_reasoning` is the state the **generation prompt** left the model
    /// in ([`items::lead_opens_reasoning`]), not `false`: the lead this engine
    /// submits ends inside `<think>` for every dialect that has one, so a turn is
    /// reasoning from its first generated token.
    ///
    /// Within a chunk the ids are walked *first* and the text is cut against them,
    /// so a boundary arriving mid-chunk switches the channel for the remainder of
    /// that same chunk and the boundary's own literal is never streamed as content.
    /// A head's deltas therefore split exactly where `items::produce` splits the
    /// committed rows, which is §13.2b's requirement that the live view and the
    /// stored view not disagree.
    /// A turn over a **`messages` backend** — a cloud provider — instead of the
    /// local `/completion`. D10's mode 3, on the seam it reserved.
    ///
    /// What is the same: the event stream a head draws (`TurnStarted`, `Delta`,
    /// `ToolCallProposed`, `TranscriptAppended`, `TurnFinished`), the §5.7
    /// length verdicts, the salvage budget, steering at the step boundary, the
    /// `TurnOk` the harness loops on, and the token ledger as the **record** —
    /// every produced item is appended through `append_items`, tokenised with
    /// this session's vocabulary, so the store, the hash chain, resume and the
    /// compaction arithmetic work unchanged. What is different, and said: the
    /// prompt that reached the model was the transcript as messages, not those
    /// tokens; `prefix_check` is `Skipped` with the backend's own reason; cost
    /// is metered; cache figures are the provider's coarse ones.
    ///
    /// An urgent steering message closes the connection mid-stream and the
    /// turn is `Guard`-shaped: nothing is committed, the next attempt re-sends
    /// the same messages.
    pub fn run_turn_messages(
        &mut self,
        session: &mut Session,
        sink: &mut dyn EventSink,
        steering: &mut dyn SteeringSource,
        backend: &dyn letibot_backend::MessagesBackend,
        system: &str,
        tools_json: &[String],
        max_output_tokens: Option<u32>,
    ) -> Result<TurnOk, TurnFailure> {
        use letibot_backend::{BackendError, Delta, Finish, StreamFlow, TurnRequest};

        session.turn_seq += 1;
        let turn_id = format!("{}#{}", session.transcript_id, session.turn_seq);
        let started = Instant::now();
        let model = format!("{}/{}", backend.name(), backend.model());
        sink.emit(TurnEvent::TurnStarted {
            turn_id: turn_id.clone(),
            model: model.clone(),
            ledger_head: session.ledger_head(),
            began_ms: self.turn_began_ms,
        });

        let mut pending = Pending::new();
        // Greedy, the same way and for the same reason as the local path above:
        // whatever was typed while the last tool ran is in THIS request, not the
        // one after it. On a metered provider the argument is sharper still — a
        // round generated without the operator's correction is a round they paid
        // for twice.
        // An urgent message is not queued by `absorb`; it is returned to be acted
        // on, and there is no generation here to act on yet. Held, so the stream
        // loop's first poll finds it — dropping it would turn an interrupt into
        // silence.
        if let Some(u) = pending.absorb(steering) {
            pending.hold_urgent(u);
        }
        let steering_before = pending.take_items();
        if !steering_before.is_empty() {
            session.append_items(self, &steering_before, sink)?;
        }
        let req = TurnRequest {
            system,
            tools_json,
            items: &session.items,
            max_output_tokens,
        };
        let mut urgent: Option<String> = None;
        let mut on_delta = |d: &Delta| -> StreamFlow {
            match d {
                Delta::Text(t) => sink.emit(TurnEvent::Delta {
                    turn_id: turn_id.clone(),
                    target: DeltaTarget::Text,
                    text: t.clone(),
                }),
                Delta::Reasoning(t) => sink.emit(TurnEvent::Delta {
                    turn_id: turn_id.clone(),
                    target: DeltaTarget::Reasoning,
                    text: t.clone(),
                }),
                Delta::ToolCall { .. } => {}
            }
            // §5.8's escape hatch, checked once per delta: an urgent message
            // closes the connection; the rest queue for the boundary.
            if let Some(u) = pending.absorb(steering) {
                urgent = Some(u.text);
                return StreamFlow::Stop;
            }
            StreamFlow::Continue
        };
        let done = match backend.complete(&req, &mut on_delta) {
            Ok(d) => d,
            Err(BackendError::Aborted) => {
                let reason = urgent.unwrap_or_else(|| "aborted".into());
                sink.emit(TurnEvent::TurnInterrupted {
                    turn_id: turn_id.clone(),
                    reason: "steering_urgent".into(),
                    partial_kept: false,
                });
                sink.emit(TurnEvent::TurnFinished {
                    turn_id: turn_id.clone(),
                    finish_reason: FinishReason::Aborted,
                    metrics: Box::new(self.messages_metrics(
                        &turn_id,
                        &model,
                        session,
                        None,
                        started.elapsed().as_millis() as u64,
                        FinishReason::Aborted,
                        backend,
                    )),
                });
                return Err(TurnFailure::Guard {
                    turn_id,
                    trip: crate::guards::Trip {
                        code: "steering_urgent",
                        detail: reason,
                    },
                });
            }
            // **A refusal keeps its status.** Flattening every `BackendError`
            // into `Malformed` threw away the one bit the retry needs: whether
            // taking the round again could possibly help. A provider 400 —
            // *"an assistant message with 'tool_calls' must be followed by tool
            // messages"* — arrived looking like a malformed response and was
            // retried six times, each attempt sending byte-identical bytes to be
            // refused identically, with the waits doubling.
            Err(letibot_backend::BackendError::Refused { status, body }) => {
                return Err(TurnFailure::Http(HttpError::Status { code: status, body }));
            }
            Err(e) => {
                return Err(TurnFailure::Http(HttpError::Malformed(e.to_string())));
            }
        };
        let wall_ms = started.elapsed().as_millis() as u64;

        for call in &done.tool_calls {
            sink.emit(TurnEvent::ToolCallProposed {
                turn_id: turn_id.clone(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                args_digest: args_digest(&call.arguments),
                arguments: call.arguments.clone(),
            });
        }

        let finish_reason = match done.finish {
            Finish::Length => FinishReason::Length,
            Finish::Stop | Finish::ToolCalls | Finish::Other(_) => FinishReason::Eos,
        };
        let verdict = length::classify(
            finish_reason == FinishReason::Length,
            TurnShape {
                visible_text: &done.text,
                reasoning_text: &done.reasoning,
                tool_calls: &done.tool_calls,
            },
        );
        if !verdict.may_record_as_success() {
            let metrics = self.messages_metrics(
                &turn_id,
                &model,
                session,
                Some(&done),
                wall_ms,
                finish_reason,
                backend,
            );
            sink.emit(TurnEvent::TurnFinished {
                turn_id: turn_id.clone(),
                finish_reason,
                metrics: Box::new(metrics.clone()),
            });
            return Err(match verdict {
                LengthVerdict::HardFail(reason) => {
                    sink.emit(TurnEvent::Warning {
                        code: "length_empty_turn",
                        detail: format!(
                            "finish_reason=length with {} — the turn produced nothing \
                             recordable and is failed, not shortened",
                            reason.as_str()
                        ),
                    });
                    if !self.salvage.salvaged() {
                        return Err(TurnFailure::SalvageExhausted {
                            turn_id,
                            streak: self.salvage.streak(),
                        });
                    }
                    TurnFailure::EmptyLength {
                        turn_id,
                        reason,
                        metrics: Box::new(metrics),
                    }
                }
                LengthVerdict::ToolCallsTruncated { truncated } => {
                    let notices = done
                        .tool_calls
                        .iter()
                        .map(|c| length::batch_failed_notice(&c.name))
                        .collect();
                    if !self.salvage.salvaged() {
                        return Err(TurnFailure::SalvageExhausted {
                            turn_id,
                            streak: self.salvage.streak(),
                        });
                    }
                    TurnFailure::BatchTruncated {
                        turn_id,
                        truncated,
                        notices,
                        metrics: Box::new(metrics),
                    }
                }
                other => unreachable!("{other:?} may be recorded as a success"),
            });
        }
        self.salvage.cleared();

        // The items, in the order the local dialects produce them: the thinking
        // first, then the answer with its calls.
        let mut produced = Vec::new();
        if !done.reasoning.is_empty() {
            produced.push(TranscriptItem::Reasoning {
                text: done.reasoning.clone(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            });
        }
        produced.push(TranscriptItem::Assistant {
            text: done.text.clone(),
            tool_calls: done.tool_calls.clone(),
            truncated: matches!(verdict, LengthVerdict::TruncatedText),
        });
        session.append_items(self, &produced, sink)?;

        let metrics = self.messages_metrics(
            &turn_id,
            &model,
            session,
            Some(&done),
            wall_ms,
            finish_reason,
            backend,
        );
        sink.emit(TurnEvent::TurnFinished {
            turn_id: turn_id.clone(),
            finish_reason,
            metrics: Box::new(metrics.clone()),
        });

        // **A HELD URGENT IS HELD, NOT DROPPED — the same two lines the greedy poll above uses.**
        // `absorb` returns an urgent rather than queueing it, and this site ignored the return, so an
        // interrupt that arrived when there was no generation to stop was DISCARDED here. `Pending`
        // says what that costs: *"Dropping it would turn an interrupt into silence, which is the
        // worst thing this type could do."* MEASURED as silence, on the operator's head: four presses
        // of `esc esc`, four `interrupt_idle` warnings, and no interrupt ever reached the engine.
        if let Some(u) = pending.absorb(steering) {
            pending.hold_urgent(u);
        }
        let steering_items = pending.take_items();
        if !steering_items.is_empty() {
            session.append_items(self, &steering_items, sink)?;
        }
        Ok(TurnOk::new(
            turn_id,
            produced,
            metrics,
            verdict,
            false,
            steering_items,
            steering_before,
        ))
    }

    /// Metrics for a messages turn: the provider's figures where it gave them,
    /// zero where the field is a llama.cpp fact that has no cloud counterpart,
    /// and a prefix check that says it did not run.
    fn messages_metrics(
        &self,
        turn_id: &str,
        model: &str,
        session: &Session,
        done: Option<&letibot_backend::Completion>,
        wall_ms: u64,
        finish_reason: FinishReason,
        backend: &dyn letibot_backend::MessagesBackend,
    ) -> TurnMetrics {
        let cost = done.map(|d| d.cost).unwrap_or(letibot_backend::TurnCost {
            meter: backend.caps().meter,
            prompt_tokens: 0,
            cached_tokens: 0,
            generated_tokens: 0,
            wall_ms,
            micros_usd: None,
        });
        TurnMetrics {
            turn_id: turn_id.to_string(),
            model: model.to_string(),
            dialect_template_sha: hex32(&self.spec.template_sha),
            ledger_head_sent: session.ledger_head(),
            prompt_tokens: cost.prompt_tokens,
            cached_tokens: cost.cached_tokens,
            predicted_tokens: cost.generated_tokens,
            prompt_tokens_server: cost.prompt_tokens,
            prompt_processed: cost.prompt_tokens.saturating_sub(cost.cached_tokens),
            finish_reason,
            prompt_ms: 0.0,
            predicted_ms: wall_ms as f64,
            draft_n: 0,
            draft_n_accepted: 0,
            id_slot: -1,
            n_busy_slots: None,
            prefix_check: PrefixCheck::Skipped {
                reason: backend.caps().skip_reason("I1").unwrap_or_else(|| {
                    "SKIPPED I1: a messages backend sends the transcript, not the ledger's \
                     tokens, so there is no prefix to check. This did not run and it is not \
                     a pass."
                        .into()
                }),
            },
            cost,
            wall_ms,
        }
    }

    fn stream_turn(
        &self,
        turn_id: &str,
        prompt: Vec<TokenId>,
        // The string form, when this prompt carries an image: `(prompt_string, base64 payloads)`.
        // Built by the caller because the spans it comes from are the caller's, and passed here
        // rather than recomputed so the ids and the string cannot describe two different prompts.
        multimodal: Option<(String, Vec<String>)>,
        opens_in_reasoning: bool,
        sink: &mut dyn EventSink,
        steering: &mut dyn SteeringSource,
        pending: &mut Pending,
    ) -> Result<(StreamOutcome, Option<Trip>), TurnFailure> {
        // **A turn with a picture in it travels as text** — see [`Prompt`]: llama.cpp honours
        // `multimodal_data` only for a `prompt_string`, so the server tokenizes it. `multimodal` is
        // `None` for every turn that carries no image, which is almost all of them.
        let request = match multimodal {
            Some((prompt_string, media)) => {
                CompletionRequest::with_media(prompt_string, media, self.sampling.clone())
            }
            None => CompletionRequest::new(prompt, self.sampling.clone()),
        };
        let body = http::post_json(&self.endpoint, "/completion", &request.to_json())?;

        let mut acc = IdAccumulator::new();
        let mut guards = GuardSet::new(&self.spec.guards);
        let mut in_reasoning = opens_in_reasoning;
        // The third channel, decided by id exactly as the second one is. A turn
        // never opens inside a call: the generation prompt ends in `<think>` or in
        // nothing, never in `<tool_call>`.
        let mut in_call = false;
        let mut trip: Option<Trip> = None;
        let mut abort: Option<AbortCause> = None;
        let mut final_chunk = None;
        let mut stream_err: Option<StreamError> = None;
        let mut capture = self.frame_capture.begin(turn_id);

        let streamed = body.for_each_event(|payload| {
            // Unconditional, and before anything classifies it: by the time a frame
            // is recognised as unaccountable, the frames that explain it have
            // already gone past.
            capture.observe(payload);
            if capture.is_armed() {
                // A frame was refused. Nothing more reaches the accumulator — the
                // guard has not moved — but a few more frames are read so the
                // capture can say whether the missing id turns up late.
                return Ok(if capture.trail_complete() {
                    Flow::Stop
                } else {
                    Flow::Continue
                });
            }

            let chunk = match crate::completion::classify(payload) {
                Ok(c) => c,
                Err(e) => {
                    capture.arm(e.clone());
                    stream_err = Some(StreamError::Protocol(e));
                    return Ok(Flow::Stop);
                }
            };

            if let Chunk::Progress { progress, .. } = &chunk {
                sink.emit(TurnEvent::PromptProgress {
                    turn_id: turn_id.to_string(),
                    progress: *progress,
                });
            }

            let before = acc.n_decoded();
            let fresh = match acc.push(&chunk) {
                Ok(ids) => ids.to_vec(),
                // T23. The guard is unchanged: this frame's ids are still refused,
                // and so is everything after it. What changed is that the ids
                // already accounted for are no longer thrown away with it — the
                // operator gets an interrupted turn holding a partial answer rather
                // than a failed turn holding nothing. The raw frames go to disk
                // because a one-line message has now cost two sessions.
                Err(e @ StreamError::FrameMismatch { .. }) => {
                    capture.arm(e.to_string());
                    let StreamError::FrameMismatch {
                        n_decoded,
                        previous,
                        ids,
                        reading,
                    } = e
                    else {
                        unreachable!("matched above")
                    };
                    abort = Some(AbortCause::FrameMismatch {
                        n_decoded,
                        previous,
                        ids,
                        reading,
                    });
                    return Ok(if capture.trail_complete() {
                        Flow::Stop
                    } else {
                        Flow::Continue
                    });
                }
                Err(e) => {
                    capture.arm(e.to_string());
                    stream_err = Some(e);
                    return Ok(Flow::Stop);
                }
            };

            // The server's own counter, one event per frame that advanced it. The
            // generation half of liveness: a head tells a hang from a model that is
            // still emitting by whether this moves, which is exactly the case a
            // long tool-call write is, where no visible text moves at all.
            if acc.n_decoded() > before {
                sink.emit(TurnEvent::TokensGenerated {
                    turn_id: turn_id.to_string(),
                    tokens: acc.n_decoded(),
                });
            }

            // The text this chunk carries, consumed left to right as its ids are
            // walked. Never re-derived from the ids: the server buffers an
            // incomplete UTF-8 character across frames, and detokenizing a chunk
            // ourselves would undo that. What the ids decide is *where* it is cut
            // and *which channel* each piece is announced on.
            let mut rest: &str = match &chunk {
                Chunk::Token { text, .. } => text.as_str(),
                _ => "",
            };
            let mut halt = false;

            for id in fresh {
                let role = self.control.role_of(id);
                if role.is_some()
                    && let Some(literal) = self.control_literal(id)
                    && let Some(at) = rest.find(literal)
                {
                    // Everything before the boundary belongs to the channel that
                    // was open when it was generated; the boundary's own literal
                    // belongs to neither. The parser drops it from the committed
                    // row, so a head that renders it is showing markup the
                    // transcript does not contain.
                    //
                    // The first occurrence, which is the right one whenever a
                    // frame carries a single token — the normal case, and always
                    // the case without a draft model. A multi-token frame in which
                    // the *text* preceding the boundary also spells the boundary
                    // out would cut early; the pieces stay on the same channel and
                    // only the spelled-out copy is swallowed.
                    emit_delta(sink, turn_id, channel(in_reasoning, in_call), &rest[..at]);
                    rest = &rest[at + literal.len()..];
                }
                match role {
                    Some(ControlRole::ThinkOpen) => in_reasoning = true,
                    Some(ControlRole::ThinkClose) => in_reasoning = false,
                    Some(ControlRole::ToolCallOpen) => in_call = true,
                    Some(ControlRole::ToolCallClose) => in_call = false,
                    _ => {}
                }
                if let Some(t) = guards.observe(id, role) {
                    trip = Some(t.clone());
                    abort = Some(AbortCause::Guard(t.code.to_string()));
                    halt = true;
                    break;
                }
                if self.client_stop_ids.contains(&id) {
                    // A stop enforced by id, not by matching decoded text — and
                    // only for stops the server will not act on itself.
                    abort = Some(AbortCause::StopToken(id));
                    halt = true;
                    break;
                }
            }
            // Flushed on the way out too, so that aborting mid-chunk cannot drop
            // characters a head would otherwise have been shown — which would be
            // this same defect with the sign flipped. What a head does with the
            // tail of an aborted turn is settled by `TurnInterrupted{partial_kept}`
            // and not by withholding deltas: a guard trip commits nothing and says
            // so, while a steering interrupt keeps its partial.
            emit_delta(sink, turn_id, channel(in_reasoning, in_call), rest);
            if halt {
                return Ok(Flow::Stop);
            }

            // §5.8's escape hatch, checked once per frame: an urgent message
            // interrupts at the next token rather than at the step boundary.
            if let Some(urgent) = pending.absorb(steering) {
                abort = Some(AbortCause::Steering(urgent.text));
                return Ok(Flow::Stop);
            }

            if let Chunk::Final(f) = chunk {
                final_chunk = Some(*f);
                return Ok(Flow::Stop);
            }
            Ok(Flow::Continue)
        });
        // Before the `?`: a socket that died mid-frame is one of the things the
        // capture exists for, so it must survive the transport error.
        report_capture(&capture, sink);
        streamed?;

        if let Some(e) = stream_err {
            return Err(TurnFailure::Stream(e));
        }
        let final_chunk = match final_chunk {
            Some(f) => f,
            None if abort.is_some() => {
                // We closed the socket, so there is no terminal frame. Synthesise
                // one that says so rather than inventing an `eos`.
                crate::completion::FinalChunk {
                    id_slot: -1,
                    finish_reason: FinishReason::Aborted,
                    stopping_word: String::new(),
                    prompt_truncated: false,
                    n_prompt_tokens: 0,
                    n_decoded: acc.ids().len() as u64,
                    slot_tokens_after: 0,
                    timings: acc.last_timings().unwrap_or_default(),
                    ids: Vec::new(),
                    text: acc.text().to_string(),
                }
            }
            None => return Err(TurnFailure::Stream(StreamError::NoFinalChunk)),
        };

        let outcome = acc.finish(final_chunk, abort)?;
        Ok((outcome, trip))
    }

    #[allow(clippy::too_many_arguments)]
    fn metrics_for(
        &self,
        turn_id: &str,
        session: &Session,
        outcome: &StreamOutcome,
        prompt_tokens: u64,
        wall_ms: u64,
        finish_reason: FinishReason,
        prefix_check: PrefixCheck,
    ) -> TurnMetrics {
        let t = &outcome.final_chunk.timings;
        TurnMetrics {
            turn_id: turn_id.to_string(),
            model: self.model.clone(),
            dialect_template_sha: hex32(&self.spec.template_sha),
            ledger_head_sent: session.ledger_head(),
            prompt_tokens,
            cached_tokens: t.cache_n,
            predicted_tokens: outcome.ids.len() as u64,
            // The server's own two numbers, kept so C10 is a comparison rather
            // than a tautology. See `TurnMetrics::prompt_tokens_server`.
            prompt_tokens_server: outcome.final_chunk.n_prompt_tokens,
            prompt_processed: t.prompt_n,
            finish_reason,
            prompt_ms: t.prompt_ms,
            predicted_ms: t.predicted_ms,
            draft_n: t.draft_n,
            draft_n_accepted: t.draft_n_accepted,
            id_slot: outcome.final_chunk.id_slot,
            // §18.2: "a tok/s number without its concurrency is not a number." The
            // engine does not read `/slots` on the turn path — a second request per
            // turn against a box whose slots are the contended resource is the
            // wrong trade — so this is absent rather than an invented 1, and a rate
            // computed from these metrics must say so.
            n_busy_slots: None,
            prefix_check,
            cost: cost_from(self.caps.meter, t, prompt_tokens, wall_ms),
            wall_ms,
        }
    }
}

/// One `Delta`, on a channel that has already been decided.
///
/// Free-standing and taking the channel as a value rather than reading it off the
/// engine, because the whole of T12 was that this call used to happen *before* the
/// value it needs had been computed.
fn emit_delta(sink: &mut dyn EventSink, turn_id: &str, target: DeltaTarget, text: &str) {
    if text.is_empty() {
        return;
    }
    sink.emit(TurnEvent::Delta {
        turn_id: turn_id.to_string(),
        target,
        text: text.to_string(),
    });
}

/// Flush a T23 capture and say where it went.
///
/// The path is announced on the event stream rather than only written, because a
/// capture nobody is told about is a file in a temp directory nobody looks in —
/// which is the position T23 has been in since it was first seen.
///
/// A capture that cannot be written is a warning and nothing more. Turning a
/// diagnostic's failure into a turn's failure would be this defect a second time.
fn report_capture(capture: &crate::capture::CaptureSession, sink: &mut dyn EventSink) {
    if !capture.is_armed() {
        return;
    }
    match capture.write() {
        Ok(Some(path)) => sink.emit(TurnEvent::Warning {
            code: "frame_capture_written",
            detail: format!(
                "the refused frame and its neighbours were written to {}",
                path.display()
            ),
        }),
        Ok(None) => sink.emit(TurnEvent::Warning {
            code: "frame_capture_disabled",
            detail: "a frame was refused and frame capture is switched off, so the \
                     evidence that would identify it was not kept"
                .into(),
        }),
        Err(e) => sink.emit(TurnEvent::Warning {
            code: "frame_capture_failed",
            detail: format!("a frame was refused but the capture could not be written: {e}"),
        }),
    }
}

/// The channel a delta belongs on, from the two boundary flags.
///
/// A call inside reasoning is still a call: the markup is never prose, wherever it
/// was written. `GuardSet` has an opinion about whether that should have happened
/// at all, and that is a separate question from what it is.
fn channel(in_reasoning: bool, in_call: bool) -> DeltaTarget {
    match (in_call, in_reasoning) {
        (true, _) => DeltaTarget::ToolCall,
        (false, true) => DeltaTarget::Reasoning,
        (false, false) => DeltaTarget::Text,
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The decoder a caller needs to read a session's tokens back.
pub fn decode_tokens(vocab: &Vocab, control: &ControlMap, tokens: &[TokenId]) -> String {
    VocabDecoder::new(vocab, control).decode(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::length::EmptyReason;

    /// The type-level half of "an empty length turn cannot be a success".
    #[test]
    #[should_panic(expected = "§5.7 says this turn failed")]
    fn a_hard_fail_verdict_cannot_be_constructed_as_a_recorded_turn() {
        let metrics = TurnMetrics {
            turn_id: "t".into(),
            model: "m".into(),
            dialect_template_sha: String::new(),
            ledger_head_sent: String::new(),
            prompt_tokens: 1,
            cached_tokens: 0,
            predicted_tokens: 0,
            prompt_tokens_server: 1,
            prompt_processed: 1,
            finish_reason: FinishReason::Length,
            prompt_ms: 0.0,
            predicted_ms: 0.0,
            draft_n: 0,
            draft_n_accepted: 0,
            id_slot: -1,
            n_busy_slots: None,
            prefix_check: PrefixCheck::FirstTurn,
            cost: cost_from(letibot_backend::Meter::WallClock, &Default::default(), 1, 0),
            wall_ms: 0,
        };
        let _ = TurnOk::new(
            "t".into(),
            vec![],
            metrics,
            LengthVerdict::HardFail(EmptyReason::ReasoningOnly),
            false,
            vec![],
            vec![],
        );
    }
}
