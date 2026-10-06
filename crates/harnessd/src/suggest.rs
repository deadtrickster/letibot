//! The daemon's model call for the smart `!`: the local endpoint, bounded.
//!
//! The prompt and the defensive parse live in `letibot_sessionlog::suggest`, where
//! they are testable without standing up inference. This is the half that talks to a
//! model, and it is the daemon's because the daemon is the one with an endpoint and
//! an HTTP client. The operator's ask, in their words: *"i want smart ! when a model
//! suggest completions."*
//!
//! # Local only, and bounded
//!
//! The endpoint is the one the daemon already resolved into `cfg.oracle` — the
//! `[gatekeeper]` endpoint in the operator's providers.toml, which on this box is the
//! LOCAL model. It is never a metered provider, because a suggestion must not cost
//! money per keystroke: a daemon with no local endpoint installs no suggester, and a
//! `SuggestShell` then answers with an empty list, which the head reads as *no
//! suggestion*.
//!
//! The call is bounded two ways. The output is capped small — the answer is at most
//! five short lines, and a cap that could hold a paragraph would let a chatty model
//! spend the operator's time on prose. And the read is cut off at a timeout, because
//! **a suggestion that does not arrive is nothing**: it is never a reason to wait, and
//! a model that overruns the deadline is abandoned and the answer is the empty list.

use std::time::Duration;

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::suggest::{self, RECENT_ROWS};
use letibot_sessionlog::ShellSuggester;
use letibot_transcript::TranscriptItem;
use letibot_turn::http::{self, Endpoint};

/// **How long a suggestion may take the local model.**
///
/// The gatekeeper's own budget is for a verdict that fails closed on a timeout, and it
/// is tight (400 ms by default) because it is on the tool path. A suggestion is off the
/// tool path — it fills a composer, it runs nothing — so it may take a little longer,
/// but it is still a completion and not a turn: three seconds is the patience of a
/// person waiting for a word to appear, and past it the answer is *nothing* rather than
/// a stall.
pub const SUGGEST_TIMEOUT: Duration = Duration::from_secs(3);

/// **How many output tokens a suggestion may spend.**
///
/// Five shell lines, one per line, is a handful of tokens. The cap is generous enough
/// for five real commands and small enough that a model that starts explaining itself
/// is cut off before the explanation costs the operator anything.
pub const SUGGEST_MAX_TOKENS: usize = 128;

/// The local model, asked for `!` completions.
pub struct LocalSuggester {
    endpoint: Endpoint,
    model: String,
}

impl LocalSuggester {
    /// `endpoint` is the local model (the `[gatekeeper]` endpoint), and `model` is the
    /// name that server serves. Both come from the daemon's own config, so a suggester
    /// and the guard never disagree about which model is local.
    pub fn new(endpoint: Endpoint, model: String) -> Self {
        LocalSuggester { endpoint, model }
    }

    /// One round trip to the local model, or `None` when it did not answer in time or
    /// at all. `None` is the whole of the failure mode: a suggestion that does not
    /// arrive is nothing, and the caller answers with an empty list.
    fn ask(&self, prompt: &str) -> Option<String> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": SUGGEST_MAX_TOKENS,
            "temperature": 0.0,
            // Both spellings, for the reason the oracle's own call sends both: which
            // one a build honours depends on its template, and a suggestion that spends
            // its budget in a thinking block is a suggestion that never arrives.
            "reasoning_effort": "none",
            "chat_template_kwargs": { "enable_thinking": false },
        })
        .to_string();

        // The timeout, made real: the read timeout is the bound that exists on this
        // transport, and a deadline the caller merely promises is not a deadline.
        let mut endpoint = self.endpoint.clone();
        endpoint.read_timeout = SUGGEST_TIMEOUT;
        let res = http::post_json(&endpoint, "/v1/chat/completions", &body).ok()?;
        let text = res.read_to_string().ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        // The chat shape nests it. `?` on each step: a reply we cannot read becomes
        // `None` -> an empty list, never an empty string the parser would then have to
        // guess at.
        v.get("choices")?
            .get(0)?
            .get("message")?
            .get("content")?
            .as_str()?
            .to_string()
            .into()
    }
}

impl ShellSuggester for LocalSuggester {
    fn suggest(&self, hub: &Hub, workspace: &str, prefix: &str) -> Vec<String> {
        // The conversation, from the session's own log: the last handful of rows
        // condensed and the commands already run, the two context halves the prompt
        // carries. A row whose body has not landed is `None` and is skipped — a
        // candidate built on a body nobody has is a candidate built on nothing.
        let snap = hub.snapshot();
        let items: Vec<TranscriptItem> = snap
            .items
            .iter()
            .filter_map(|s| s.item.clone())
            .collect();
        let recent = suggest::recent_rows(&items, RECENT_ROWS);
        let commands = suggest::commands_run(&items);
        let prompt = suggest::suggestion_prompt(&recent, &commands, workspace, prefix);
        // The model's half, and the defensive parse of whatever it said. A model that
        // did not answer, or answered with prose, is an empty list — the head reads
        // that as *no suggestion* and the operator's Tab does what it did before.
        match self.ask(&prompt) {
            Some(reply) => suggest::parse_suggestions(&reply),
            None => Vec::new(),
        }
    }
}
