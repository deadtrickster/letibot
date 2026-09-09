//! The turn engine (W6): one turn, from a transcript to a transcript.
//!
//! This is the first crate that **assembles** rather than builds: `transcript` says
//! what a conversation is, `dialect` says what a model's boundaries are,
//! `dialect-glm` renders and parses one model, `tokencore` owns the vocabulary and
//! the append-only token region, and `backend` says what a backend may be asked to
//! promise. Nothing here re-implements any of that.
//!
//! # The shape of one turn
//!
//! ```text
//!   render (RenderSpan)  →  tokenize  →  ledger append  →  submit the region
//!                                                              │
//!            append rows ← parse (Parser) ← accumulate ids ← stream back
//!                                │
//!                                └→ LengthPolicy, guards, post-flight I1, metrics
//! ```
//!
//! # What is in M1's scope, and what is deliberately not
//!
//! **In:** §5.6's `/completion` fallback with `prompt` as a token array, streamed;
//! §5.7's `LengthPolicy`; §8.5's guards; §5.8's steering; §18.1-I1's post-flight
//! assertion; §4.4's `turn_metrics` and §4.5's `TurnFinished`.
//!
//! **Out:** S0's control channel (`Submit` over a Unix socket with the shm region
//! handed across). It is deliberately off the critical path and the fallback is
//! what M1 runs on. [`completion::CompletionRequest`] is the module that would gain
//! a sibling, not a thing the engine would be rewritten around.
//!
//! # Two things measured against the running server, which the code is shaped by
//!
//! 1. **In stream mode the final frame carries an empty `tokens` array.** Generated
//!    ids exist only in the partial frames. See [`stream`].
//! 2. **`return_progress: true` — which §5.6 requires — makes llama.cpp emit
//!    progress frames carrying a fabricated token id 0.** So the obvious fix for
//!    (1) corrupts the ledger on the first turn. Also [`stream`].
//!
//! # The renderer is behind a seam, on purpose
//!
//! T1 settled that rendering will be template-driven (the model's shipped jinja
//! through minijinja). That renderer does not exist yet. The engine depends on
//! [`renderer::PromptRenderer`] and on `RenderSpan`; `GlmRenderer` is the working
//! implementation today and sits behind the same trait. When the template-driven
//! one lands, nothing in this crate changes.

pub mod completion;
pub mod engine;
pub mod events;
pub mod guards;
pub mod http;
pub mod items;
pub mod length;
pub mod metrics;
pub mod prefix;
pub mod renderer;
pub mod resume;
pub mod steering;
pub mod stream;

pub use completion::{Chunk, CompletionRequest, FinalChunk, FinishReason, PromptProgress, Timings};
pub use engine::{EngineError, Session, TurnEngine, TurnFailure, TurnOk};
pub use events::{DeltaTarget, EventSink, NullSink, RecordingSink, TurnEvent};
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
