//! Cloud providers - GLM (Zhipu), DeepSeek, Grok (xAI) - as a messages backend:
//! one OpenAI-shaped chat-completions client and one preset per door.
//! The seam carries the transcript ([`messages::convert`]). Cost is `None` for an unpriced model.

pub mod catalogue;
pub mod keys;
pub mod messages;
pub mod openai;
pub mod presets;

pub use keys::{Credentials, Gatekeeper, KeyError, MemorySection, gatekeeper, memory};
pub use openai::OpenAiProvider;
pub use presets::{Preset, Prices};
