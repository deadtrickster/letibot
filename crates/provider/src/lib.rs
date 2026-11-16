//! Cloud providers as a messages backend: one client per protocol ([`openai`], [`anthropic`],
//! [`responses`], [`gemini`]) and one preset per door. The seam carries the transcript
//! ([`messages::convert`]). Cost is `None` for an unpriced model.

pub mod anthropic;
pub mod catalogue;
pub mod effort;
pub mod gemini;
pub mod keys;
pub mod kimi;
pub mod marks;
pub mod messages;
pub mod openai;
pub mod presets;
pub mod registry;
pub mod responses;
pub mod usage;
pub mod usage_poll;
pub mod usage_warn;

pub use anthropic::AnthropicProvider;
pub use keys::{Credentials, Gatekeeper, KeyError, MemorySection, gatekeeper, memory};
pub use openai::OpenAiProvider;
pub use presets::{Preset, Prices, Protocol};
pub use responses::ResponsesProvider;

/// The wall clock the clients read a header by - one spelling, so a `Retry-After`
/// date and the caller's clock never come from two different clocks.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
