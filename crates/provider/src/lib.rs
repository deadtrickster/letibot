//! Cloud providers — GLM (Zhipu), DeepSeek, Grok (xAI) — as a
//! [`MessagesBackend`]: one OpenAI-shaped chat-completions client, three
//! presets. D10's mode 3, built on the seam it reserved.
//!
//! # What crosses the seam
//!
//! The **transcript** (`TurnRequest.items`), never a rendered prompt and never
//! token ids. [`messages::convert`] turns items into the API's messages: system,
//! user (text parts), assistant (`content` + `tool_calls`, and `reasoning_content`
//! when the provider wants its own thinking back), and `tool` results keyed by
//! `tool_call_id`. Tool schemas ride as `tools`. The engine keeps its own token
//! ledger as the local record; nothing here sees it.
//!
//! # What the providers actually do (checked against their published APIs,
//! 2026-09-14; verified live where a key was present)
//!
//! | | endpoint | reasoning | prompt cache |
//! |---|---|---|---|
//! | `deepseek` | `https://api.deepseek.com/chat/completions` | `reasoning_content` on `deepseek-reasoner`; `deepseek-chat` is plain | automatic prefix cache; `usage.prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` |
//! | `glm` | `https://open.bigmodel.cn/api/paas/v4/chat/completions` | `reasoning_content` when `thinking: {type: enabled}` | not reported per turn |
//! | `grok` | `https://api.x.ai/v1/chat/completions` | `reasoning_content` on reasoning models | `usage.prompt_tokens_details.cached_tokens` |
//!
//! Every one of them streams with `stream: true` as SSE `data:` lines and
//! finishes with `data: [DONE]`; `stream_options: {include_usage: true}` puts the
//! `usage` object on the last chunk. Tool calls arrive as deltas keyed by
//! `index`, the first carrying `id` and `function.name`, every one appending to
//! `function.arguments`.
//!
//! # Token saving, and where it lives
//!
//! A metered provider changes the arithmetic the operator named: compaction
//! and summarisation should be more eager, and the providers' own prefix caches
//! reward a stable prefix — which the harness already keeps (`StablePrefix`,
//! message 0 never rewritten). Cache hits are surfaced in [`TurnCost::cached_tokens`]
//! so the operator can see whether the prefix is holding. The eager-compaction
//! policy itself is the harness's knob, not this crate's.
//!
//! # Cost is a table, not a memory
//!
//! Prices change and a number recalled is a number invented. `micros_usd` is
//! computed only when the operator's `providers.toml` prices the model
//! (`[prices."deepseek-chat"] input=…, cached=…, output=…`, USD per million);
//! otherwise it is `None` — unpriced, not free — exactly D10's reason for the
//! `Option`.

pub mod keys;
pub mod messages;
pub mod openai;
pub mod presets;

pub use keys::{Credentials, Gatekeeper, KeyError, gatekeeper};
pub use openai::OpenAiProvider;
pub use presets::{Preset, Prices};
