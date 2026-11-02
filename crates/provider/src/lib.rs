//! Cloud providers - GLM (Zhipu), DeepSeek, Grok (xAI) - as a messages backend:
//! one OpenAI-shaped chat-completions client and one preset per door.
//!
//! What crosses the seam is the transcript, never a rendered prompt and never token
//! ids; [`messages::convert`] does that translation. Cost comes from the operator's
//! price table and is `None` for an unpriced model, because unpriced is not free - a
//! number recalled is a number invented. Every provider streams SSE `data:` lines
//! ending in `data: [DONE]`, with `usage` on the last chunk.

pub mod catalogue;
pub mod keys;
pub mod messages;
pub mod openai;
pub mod presets;

pub use keys::{Credentials, Gatekeeper, KeyError, MemorySection, gatekeeper, memory};
pub use openai::OpenAiProvider;
pub use presets::{Preset, Prices};
