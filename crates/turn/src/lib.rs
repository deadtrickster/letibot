//! The turn engine: one turn, from a transcript to a transcript.
//!
//! This crate assembles what the others define - `transcript` says what a
//! conversation is, `dialect` what a model's boundaries are, `tokencore` owns the
//! vocabulary and the append-only region, `backend` what a backend may promise -
//! and re-implements none of it.
//!
//! ```text
//!   render (RenderSpan)  →  tokenize  →  ledger append  →  submit the region
//!                                                              │
//!            append rows ← parse (Parser) ← accumulate ids ← stream back
//!                                │
//!                                └→ LengthPolicy, guards, post-flight I1, metrics
//! ```
//!
//! Two facts about the stream shape the code: in stream mode the final frame
//! carries an empty `tokens` array, and `return_progress: true` makes llama.cpp
//! emit progress frames carrying a fabricated token id 0 - so taking ids from
//! either corrupts the ledger. See [`stream`].
//!
//! Rendering is behind [`renderer::PromptRenderer`] so a template-driven renderer
//! can replace `GlmRenderer` without this crate changing.

pub mod capture;
pub mod compaction;
pub mod completion;
pub mod engine;
pub mod events;
pub mod evict;
pub mod fidelity;
pub mod guards;
pub mod http;
pub mod items;
pub mod length;
pub mod metrics;
pub mod prefix;
pub mod prompt_cache;
pub mod renderer;
pub mod resume;
pub mod serving;
pub mod steering;
pub mod stream;

pub use capture::{CaptureSession, FrameCapture};
pub use compaction::{
    CompactionOutcome, Harvest, OverrunPlan, SUMMARY_FIDELITY_PER_MILLE, SUMMARY_INSTRUCTION,
    SUMMARY_SECTIONS, SummaryAsk, SummaryBudget, TAIL_BECAUSE, TailPlan, TailSplit,
    UNFINISHED_REASONING_NOTICE, WIRE_SECTIONS, WRITE_ROOM, carried_text, fold_tail_room,
    parse_summary_sections, plan_compaction_tail, plan_cut, plan_fold, plan_overrun, plan_tail,
    run_compaction, summarise_first_half, summarise_overrun, summary_budget, summary_instruction,
    tail_because, tail_budget, tail_split_of, undo_refused_summary,
};
pub use completion::{Chunk, CompletionRequest, FinalChunk, FinishReason, PromptProgress, Timings};
pub use engine::{EngineError, Session, TurnEngine, TurnFailure, TurnOk};
pub use events::{DeltaTarget, EventSink, NullSink, RecordingSink, TurnEvent};
pub use fidelity::{Facts, Fidelity, Level};
pub use guards::{GuardSet, Trip};
pub use http::{Endpoint, HttpError};
pub use items::{Produced, ProducedItem};
pub use length::{EmptyReason, LengthVerdict, SalvageBudget, TurnShape};
pub use metrics::TurnMetrics;
pub use prefix::{PrefixCheck, PrefixWitness};
pub use renderer::PromptRenderer;
pub use resume::RestoreError;
pub use steering::{ChannelSteering, NoSteering, SteeringMessage, SteeringSource};
pub use stream::{AbortCause, IdAccumulator, StreamError, StreamOutcome};
