//! Cloud providers as a messages backend: one client per protocol ([`openai`], [`anthropic`],
//! [`responses`], [`gemini`]) and one preset per door. The seam carries the transcript
//! ([`messages::convert`]). Cost is `None` for an unpriced model.

pub mod agent;
pub mod anthropic;
pub mod catalogue;
pub mod effort;
pub mod gemini;
pub mod keys;
pub mod kimi;
pub mod marks;
pub mod messages;
pub mod name;
pub mod openai;
pub mod presets;
pub mod refresh;
pub mod registry;
pub mod responses;
pub mod usage;
pub mod usage_poll;
pub mod usage_warn;

#[cfg(test)]
pub mod testing;

pub use anthropic::AnthropicProvider;
pub use keys::{Credentials, Gatekeeper, KeyError, MemorySection, gatekeeper, memory};
pub use openai::OpenAiProvider;
pub use presets::{Preset, Prices, Protocol};
pub use responses::ResponsesProvider;
