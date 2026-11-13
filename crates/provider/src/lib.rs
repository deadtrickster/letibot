//! Cloud providers - GLM (Zhipu), DeepSeek, Grok (xAI), Anthropic, OpenAI and the compatible coding
//! doors - as a messages backend: one client per PROTOCOL ([`openai`] for chat completions,
//! [`anthropic`] for Messages, [`responses`] for OpenAI's Responses) and one preset per door. The
//! seam carries the transcript ([`messages::convert`]). Cost is `None` for an unpriced model.

pub mod anthropic;
pub mod catalogue;
pub mod keys;
pub mod marks;
pub mod messages;
pub mod openai;
pub mod presets;
pub mod responses;

pub use anthropic::AnthropicProvider;
pub use keys::{Credentials, Gatekeeper, KeyError, MemorySection, gatekeeper, memory};
pub use openai::OpenAiProvider;
pub use presets::{Preset, Prices, Protocol};
pub use responses::ResponsesProvider;
