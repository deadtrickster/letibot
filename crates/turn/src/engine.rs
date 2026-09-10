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
use letibot_dialect::{ControlRole, DialectSpec, Parser, RenderSpan, StablePrefix, TokenDecoder};
use letibot_tokencore::{
    ControlMap, TokenId, TokenLedger, Vocab, VocabDecoder, resolve, resolve_stops, tokenize_spans,
};
use letibot_transcript::{ReasoningField, TranscriptItem};
use serde_json::Value;

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
}

impl TurnOk {
    fn new(
        turn_id: String,
        items: Vec<TranscriptItem>,
        metrics: TurnMetrics,
        verdict: LengthVerdict,
        interrupted: bool,
        steering_applied: Vec<TranscriptItem>,
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
    /// Which wire field this model replays its own reasoning into.
    ///
    /// It belongs on `DialectSpec` — it is a per-model fact of exactly the kind
    /// that crate models as data — but the type does not carry it yet, so the
    /// engine takes it as configuration rather than guessing.
    pub reasoning_field: ReasoningField,
    pub sampling: Value,
    pub salvage: SalvageBudget,
}

impl<'a> TurnEngine<'a> {
    /// Resolve the dialect against the vocabulary and fail **now** if it does not
    /// fit. Every failure this can raise is one that is otherwise silent at
    /// runtime.
    ///
    /// Eight arguments, and a builder would be worse: every one of them is a fact
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
        reasoning_field: ReasoningField,
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
            reasoning_field,
            sampling,
            salvage: SalvageBudget::default(),
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
        });

        // Shared with the stream loop: a non-urgent message that arrives mid-turn
        // is absorbed there and injected here. Two `Pending`s would mean the queue
        // that saw the message is not the queue the step boundary drains, and the
        // correction would be silently dropped — which is worse than not
        // implementing steering at all.
        let mut pending = Pending::new();
        let lead = self.tokenize(&self.renderer.generation_prompt())?;
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
        let (outcome, guard_trip) = self.stream_turn(
            &turn_id,
            prompt.clone(),
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
        let produced = items::produce(
            &lead,
            &outcome.ids,
            &self.stop_ids,
            self.parser,
            &decoder,
            self.reasoning_field,
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
        self.salvage.cleared();

        // Commit. The rows are cut out of the id array the server streamed; nothing
        // is re-rendered and nothing is re-tokenized.
        let mut appended = Vec::new();
        for (n, produced_item) in produced.items.iter().enumerate() {
            let item_id = format!("{turn_id}.{n}");
            let row = session
                .ledger
                .append(&item_id, &produced.tokens[produced_item.range.clone()])
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
            sink.emit(TurnEvent::Warning { code, detail });
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

        // §5.8's escape hatch fired: generation stopped at the next token and the
        // partial output was kept. Announced as an interruption, because a kept
        // partial that reports as a complete turn is §5.7's failure with a
        // different cause.
        let interrupted = matches!(outcome.aborted, Some(AbortCause::Steering(_)));
        if interrupted {
            sink.emit(TurnEvent::TurnInterrupted {
                turn_id: turn_id.clone(),
                reason: "steering_urgent".to_string(),
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
        pending.absorb(steering);
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
    fn stream_turn(
        &self,
        turn_id: &str,
        prompt: Vec<TokenId>,
        opens_in_reasoning: bool,
        sink: &mut dyn EventSink,
        steering: &mut dyn SteeringSource,
        pending: &mut Pending,
    ) -> Result<(StreamOutcome, Option<Trip>), TurnFailure> {
        let request = CompletionRequest::new(prompt, self.sampling.clone());
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

        body.for_each_event(|payload| {
            let chunk = match crate::completion::classify(payload) {
                Ok(c) => c,
                Err(e) => {
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

            let fresh = match acc.push(&chunk) {
                Ok(ids) => ids.to_vec(),
                Err(e) => {
                    stream_err = Some(e);
                    return Ok(Flow::Stop);
                }
            };

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
        })?;

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
        );
    }
}
