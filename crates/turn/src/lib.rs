//! The turn engine: one turn, from a transcript to a transcript. It assembles `transcript`,
//! `dialect`, `tokencore` and `backend` and re-implements none of them. Rendering is behind
//! [`renderer::PromptRenderer`]; stream ids follow the rules in [`stream`].

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
    CHARS_PER_TOKEN, CompactionOutcome, Harvest, MAX_SUMMARY_TOKENS, MIN_SUMMARY_TOKENS,
    OverrunPlan, PINNED_PER_MILLE, PROGRESS_EVERY_MS, SUMMARY_FIDELITY_PER_MILLE,
    SUMMARY_INSTRUCTION, SUMMARY_SECTIONS, Section, SummaryAsk, SummaryBudget, TAIL_BECAUSE,
    TailPlan, TailSplit, Template, UNFINISHED_REASONING_NOTICE, WIRE_SECTIONS, WRITE_ROOM,
    carried_text, fold_tail_room, instruction_with, parse_summary_sections, plan_compaction_tail,
    plan_cut, plan_fold, plan_overrun, plan_tail, run_compaction, run_compaction_as_progress,
    summarise_first_half, summarise_overrun, summary_budget, summary_instruction, tail_because,
    tail_budget, tail_split_of, undo_refused_summary,
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
