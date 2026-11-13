//! The Anthropic Messages client: `POST /v1/messages`, `anthropic-version`, SSE.
//!
//! The second protocol this crate speaks, beside the OpenAI-shaped chat completions in
//! [`crate::openai`]. The two differ in more than spelling, and each difference is a rule the API
//! enforces rather than a style: the system prompt is a TOP-LEVEL field instead of a message
//! (<https://platform.claude.com/docs/en/api/messages>), a tool result is a `tool_result` BLOCK in
//! a user turn that has to answer a `tool_use` in the message directly above it, and a thinking
//! block carries a `signature` that must go back unmodified or the request is refused
//! (<https://platform.claude.com/docs/en/build-with-claude/thinking#preserving-thinking-blocks>).
//! The transcript already holds all of it: see [`crate::anthropic::request`] for the mapping, row
//! by row, and [`crate::anthropic::stream`] for the events that build a turn back.
//!
//! Three decisions are this module's rather than the API's, and each is the smaller of two losses:
//!
//! * `anthropic-version: 2023-06-01`. Every model still answers it, every Anthropic-compatible
//!   coding endpoint was built against it, and it is the last version where `temperature`, `top_p`
//!   and `top_k` exist - the operator's sampling is passed through, and `2026-10-21` refuses those
//!   three outright (<https://platform.claude.com/docs/en/api/versioning>).
//! * `thinking` is asked for as `{"type": "adaptive"}`, the current mode; a model that only has the
//!   old manual mode says so in a 400 ("adaptive thinking is not supported on this model") and the
//!   request goes once more with `{"type": "enabled", "budget_tokens": N}`. The reverse of that
//!   trade is a hard-coded model-family guess, and this tree does not price a model by its name.
//! * A turn that asks NOT to think sends no `thinking` field at all. `{"type": "disabled"}` is a
//!   400 on the models whose thinking is always on, and omitting the field is the only request
//!   every model reads (<https://platform.claude.com/docs/en/api/errors>).
//!
//! Two refusals are answered by retrying the same request once, because the API documents the
//! remedy and the request is where it is applied: adaptive thinking unsupported (above), and a
//! thinking block the API will not accept because the history it was signed against has changed -
//! the documented fix is to strip it and everything after it, which is every block this request
//! carries (<https://platform.claude.com/docs/en/api/errors>).

pub mod request;
pub mod stream;

#[cfg(test)]
mod tests;

use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use letibot_backend::{
    BackendCaps, BackendError, Completion, Delta, MessagesBackend, StreamFlow, TurnRequest,
};
use serde_json::{Value, json};

use crate::catalogue::Catalogue;
use crate::keys::Credentials;
use crate::messages::{Eyes, Withheld};
use crate::openai::{DEFAULT_ANSWER_TIMEOUT, DEFAULT_FIRST_BYTE, Notes};
use crate::presets::Preset;

/// The version every model and every compatible endpoint reads, and the last one where the
/// operator's `temperature`/`top_p`/`top_k` are still fields: `2026-10-21` removes them.
pub const API_VERSION: &str = "2023-06-01";

/// `max_tokens` is REQUIRED and has no default of its own. This is the number a request gets when
/// neither the turn nor the operator's sampling names one, and it has to leave room for thinking:
/// thinking tokens are counted against `max_tokens`.
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

/// The smallest `budget_tokens` the manual thinking mode accepts.
const MIN_BUDGET: u32 = 1024;

/// How a turn asked for thinking, in the three states this API has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Thinking {
    /// Not asked for: the field is omitted and the model does what it does by default.
    Off,
    /// `{"type": "adaptive"}` - the model decides how much to think.
    Adaptive,
    /// `{"type": "enabled", "budget_tokens": N}` - the mode models before 4.7 have, and the mode
    /// the API asks for by name when it refuses `adaptive`.
    Manual,
}

/// One provider, one model, one key.
pub struct AnthropicProvider {
    preset: &'static Preset,
    model: String,
    url: String,
    creds: Credentials,
    agent: ureq::Agent,
    /// The two deadlines `agent` was built from, kept so setting one never resets the other.
    first_byte: Duration,
    answer: Duration,
    /// Ask the provider to think out loud, where it has a switch for it.
    pub thinking: bool,
    /// `temperature` and friends, passed through where the API still has the field.
    pub sampling: Value,
    /// The catalogue this provider looks a model up in: the box's by default, and a caller's
    /// own when a test must not read the machine's files.
    catalogue: std::sync::Arc<Catalogue>,
    /// Where a fact the operator must read is said: the daemon's hub, through the same closure
    /// the OpenAI client uses.
    notes: Option<Notes>,
}

impl AnthropicProvider {
    pub fn new(preset: &'static Preset, model: Option<&str>, creds: Credentials) -> Self {
        let url = creds.url.clone().unwrap_or_else(|| preset.url.to_string());
        let catalogue = Catalogue::load();
        AnthropicProvider {
            preset,
            model: model
                .map(str::to_string)
                .unwrap_or_else(|| preset.default_model(&catalogue)),
            url,
            creds,
            catalogue,
            agent: Self::agent(DEFAULT_FIRST_BYTE, DEFAULT_ANSWER_TIMEOUT),
            first_byte: DEFAULT_FIRST_BYTE,
            answer: DEFAULT_ANSWER_TIMEOUT,
            thinking: false,
            sampling: json!({}),
            notes: None,
        }
    }

    /// The HTTP agent, with the two numbers that decide when a provider that has said nothing is a
    /// failure rather than a hang - the same pair the OpenAI client uses, for the same reason.
    fn agent(first_byte: Duration, answer: Duration) -> ureq::Agent {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(first_byte))
            .timeout_global(None)
            .timeout_recv_body(Some(answer))
            .build();
        ureq::Agent::new_with_config(config)
    }

    /// How long this provider may take to answer at all, over the default of
    /// [`DEFAULT_ANSWER_TIMEOUT`].
    pub fn with_response_timeout(mut self, answer: Duration) -> Self {
        self.answer = answer;
        self.agent = Self::agent(self.first_byte.min(answer), answer);
        self
    }

    /// How long this provider may stay silent before its first byte.
    pub fn with_first_byte(mut self, first_byte: Duration) -> Self {
        self.first_byte = first_byte;
        self.agent = Self::agent(first_byte.min(self.answer), self.answer);
        self
    }

    /// The catalogue this provider reads, instead of the box's.
    pub fn with_catalogue(mut self, catalogue: std::sync::Arc<Catalogue>) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// Install the way this provider tells a person something - see [`Notes`].
    pub fn with_notes(mut self, notes: Notes) -> Self {
        self.notes = Some(notes);
        self
    }

    /// What this model gets pictures as: a provider that already refused one, then the catalogue's
    /// `modalities.input`, and otherwise the pictures are sent - the same three answers, from the
    /// same table, as the OpenAI client.
    fn eyes(&self) -> Eyes<'_> {
        if crate::messages::known_text_only(self.preset.name, &self.model) {
            return Eyes::TextOnly(&self.model);
        }
        match self
            .catalogue
            .takes_images(self.preset.catalogue_id, &self.model)
        {
            Some(true) => Eyes::Sees,
            Some(false) => Eyes::TextOnly(&self.model),
            None => Eyes::Unknown(&self.model),
        }
    }

    /// The request body, for a test to look at.
    pub fn body(&self, req: &TurnRequest<'_>) -> Result<Value, BackendError> {
        self.body_for(
            req,
            request::Policy {
                eyes: self.eyes(),
                withheld: Withheld::nothing(),
                replay_thoughts: true,
            },
            self.thinking_mode(req),
        )
    }

    /// Which thinking mode this request asks for - see [`Thinking`].
    fn thinking_mode(&self, req: &TurnRequest<'_>) -> Thinking {
        match req.thinking.unwrap_or(self.thinking) {
            true => Thinking::Adaptive,
            false => Thinking::Off,
        }
    }

    fn body_for(
        &self,
        req: &TurnRequest<'_>,
        policy: request::Policy<'_>,
        thinking: Thinking,
    ) -> Result<Value, BackendError> {
        let tools = request::tools(req.tools_json).map_err(BackendError::Malformed)?;
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens(req),
            "stream": true,
            "messages": request::messages(req.items, req.tail, policy),
        });
        if let Some(system) = request::system(req.system, req.items, true) {
            body["system"] = system;
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        let max_tokens = body["max_tokens"]
            .as_u64()
            .unwrap_or(u64::from(DEFAULT_MAX_TOKENS)) as u32;
        match thinking {
            Thinking::Off => {}
            // `disabled` is a 400 where thinking is always on, so not asking is the request every
            // model reads. `summarized` is the display that still gives a head something to draw.
            Thinking::Adaptive => {
                body["thinking"] = json!({"type": "adaptive", "display": "summarized"})
            }
            Thinking::Manual => {
                // `budget_tokens` must be at least 1024 and less than `max_tokens`, and a request
                // that cannot satisfy both cannot think in this mode at all.
                if max_tokens > MIN_BUDGET {
                    let budget = (max_tokens / 2).max(MIN_BUDGET).min(max_tokens - 1);
                    body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
                }
            }
        }
        // The operator's sampling, narrowed to the fields this API has. Anything else - or a
        // `temperature` under a version that removed it - would refuse the whole request for a
        // knob this tree merely stores.
        if let Some(obj) = self.sampling.as_object() {
            for k in ["temperature", "top_p", "top_k", "stop_sequences"] {
                if let Some(v) = obj.get(k) {
                    body[k] = v.clone();
                }
            }
        }
        Ok(body)
    }

    /// The output cap: the turn's own, then the operator's, then the default. Thinking counts
    /// against this number, so it is a ceiling on the whole answer rather than on its text.
    fn max_tokens(&self, req: &TurnRequest<'_>) -> u32 {
        let asked = self
            .sampling
            .get("max_tokens")
            .and_then(Value::as_u64)
            .map(|n| n.min(u64::from(u32::MAX)) as u32);
        match (req.max_output_tokens, asked) {
            (Some(turn), Some(set)) => turn.min(set),
            (Some(turn), None) => turn,
            (None, Some(set)) => set,
            (None, None) => DEFAULT_MAX_TOKENS,
        }
    }

    /// One attempt at the wire: post the body, stream the answer. Split out because a refusal the
    /// API documents a remedy for takes the round again, and a non-2xx status returns before a byte
    /// of the body is read as a stream, which is what makes the retry safe.
    fn round(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
        policy: request::Policy<'_>,
        thinking: Thinking,
    ) -> Result<Completion, BackendError> {
        let body = self.body_for(req, policy, thinking)?;
        let payload = body.to_string();
        let started = Instant::now();
        // A signal is not the provider failing: the same one retry the OpenAI client takes.
        let mut interrupted = false;
        // `ANTHROPIC_AUTH_TOKEN` is a bearer token: a proxy reached by base URL reads that header.
        let bearer = (self.creds.from == "$ANTHROPIC_AUTH_TOKEN")
            .then(|| format!("Bearer {}", self.creds.key));
        let resp = loop {
            let mut post = self.agent.post(&self.url);
            if let Some(b) = &bearer {
                post = post.header("Authorization", b);
            }
            match post
                .header("x-api-key", &self.creds.key)
                .header("anthropic-version", API_VERSION)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                // Kimi's plan admits coding clients by this header (`kimi-coding` is a Messages
                // door to it), and our own name is the honest one on every other door.
                .header("User-Agent", crate::kimi::USER_AGENT)
                .send(payload.as_bytes())
            {
                Ok(resp) => break resp,
                Err(ureq::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::Interrupted && !interrupted =>
                {
                    interrupted = true;
                }
                Err(e) => return Err(BackendError::unreachable(e.to_string(), io_kind(&e))),
            }
        };
        let status = resp.status().as_u16();
        // The provider's own `Retry-After`, the only form of *when may I come back* a caller may
        // sleep on (<https://platform.claude.com/docs/en/api/rate-limits>), read before the body
        // consumes the response.
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        // Every response carries one, and an error body is what support asks for by it
        // (<https://platform.claude.com/docs/en/api/errors>).
        let request_id = resp
            .headers()
            .get("request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if !(200..300).contains(&status) {
            let text = resp
                .into_body()
                .read_to_string()
                .unwrap_or_else(|e| format!("(unreadable body: {e})"));
            return Err(BackendError::Refused {
                status,
                body: refusal_text(&text, request_id.as_deref()),
                retry_after,
            });
        }
        let reader = BufReader::new(resp.into_body().into_reader());
        let mut acc = stream::Accumulator::default();
        let mut aborted = false;
        for line in reader.lines() {
            let line = line.map_err(|e| {
                BackendError::unreachable(format!("mid-stream: {e}"), Some(e.kind()))
            })?;
            // The name on the `event:` line repeats the payload's own `type`, so the payload is
            // the one place this reader looks, and a name it does not know is not a failure.
            let Some(data) = line.trim_end().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            let event: Value = serde_json::from_str(data)
                .map_err(|e| BackendError::Malformed(format!("{e} in event {data}")))?;
            // The headers carried a 200 before this byte, so an `error` event is the only place a
            // mid-stream refusal can arrive; dropped, it left a half answer standing.
            if let Some(refusal) = in_band_refusal(&event, retry_after) {
                return Err(refusal);
            }
            let stopping = event.get("type").and_then(Value::as_str) == Some("message_stop");
            for d in acc.absorb(&event) {
                if on_delta(&d) == StreamFlow::Stop {
                    aborted = true;
                    break;
                }
            }
            if aborted || stopping {
                break;
            }
        }
        if aborted {
            return Err(BackendError::Aborted);
        }
        if !acc.ended() {
            // A half answer is not an answer: recorded as a stop with zero usage it was billed as
            // nothing and shown as if the model had finished the thought.
            return Err(BackendError::unreachable(
                "the stream closed without `message_stop`, so what arrived is a fragment"
                    .to_string(),
                None,
            ));
        }
        let wall_ms = started.elapsed().as_millis() as u64;
        Ok(acc.finish(
            &self.model,
            &self.creds,
            wall_ms,
            self.preset,
            &self.catalogue,
        ))
    }

    /// Tell the operator something, the daemon's log first: a session may have no head attached.
    fn note(&self, said: String) {
        eprintln!("  {said}");
        if let Some(notes) = &self.notes {
            notes(said);
        }
    }

    /// The body of a 400 that says the model has no adaptive thinking, in the words the API uses.
    fn adaptive_refused(body: &str) -> bool {
        let b = body.to_ascii_lowercase();
        b.contains("adaptive thinking") && b.contains("not supported")
    }

    /// The body of a 400 that says a thinking block cannot be replayed: modified, unsigned, or
    /// signed against a different conversation. The documented remedy for all three is to strip
    /// the block and every one after it, which for this request is every block it carries.
    fn thoughts_refused(body: &str) -> bool {
        let b = body.to_ascii_lowercase();
        b.contains("thinking")
            && (b.contains("cannot be modified")
                || b.contains("invalid `signature`")
                || b.contains("bound to a different conversation"))
    }
}

/// The io failure under a transport error, when there is one: a deadline must stay a deadline for
/// a caller that branches on it.
fn io_kind(err: &ureq::Error) -> Option<std::io::ErrorKind> {
    match err {
        ureq::Error::Timeout(_) => Some(std::io::ErrorKind::TimedOut),
        ureq::Error::Io(e) => Some(e.kind()),
        _ => None,
    }
}

/// The API's error body as one sentence, with the machine-readable type kept beside the message -
/// the same projection the OpenAI client makes of `error.code`, for the same reason: a caller
/// classifies on the word, a person reads the sentence. `request_id` is what a report to the
/// vendor needs and is useless once dropped.
fn refusal_text(text: &str, request_id: Option<&str>) -> String {
    let parsed: Option<Value> = serde_json::from_str(text).ok();
    let err = parsed.as_ref().and_then(|v| v.get("error"));
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| text.to_string());
    let said = match err.and_then(|e| e.get("type")).and_then(Value::as_str) {
        Some(kind) if !message.contains(kind) => format!("{message} ({kind})"),
        _ => message,
    };
    match request_id {
        Some(id) if !id.is_empty() => format!("{said} [request-id {id}]"),
        _ => said,
    }
}

/// The refusal the provider sends INSIDE its stream, which has no status of its own: the response's
/// is `200` because the headers went out first. `overloaded_error` is a 529 in a non-streaming
/// context, `rate_limit_error` a 429, and the rest are the status the errors page names for them
/// (<https://platform.claude.com/docs/en/api/errors>).
fn in_band_refusal(event: &Value, retry_after: Option<u64>) -> Option<BackendError> {
    if event.get("type").and_then(Value::as_str) != Some("error") {
        return None;
    }
    let err = event.get("error").unwrap_or(event);
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| err.to_string());
    let kind = err.get("type").and_then(Value::as_str).unwrap_or_default();
    let body = if kind.is_empty() || message.contains(kind) {
        message
    } else {
        format!("{message} ({kind})")
    };
    Some(BackendError::Refused {
        status: status_of(kind),
        body,
        retry_after,
    })
}

/// The HTTP status an error type stands for. A type this build does not know is a 500: the provider
/// failed, and `Policy::trouble` has nothing to say about a number it does not recognise either.
fn status_of(kind: &str) -> u16 {
    match kind {
        "invalid_request_error" => 400,
        "authentication_error" => 401,
        "billing_error" => 402,
        "permission_error" => 403,
        "not_found_error" => 404,
        "conflict_error" => 409,
        "request_too_large" => 413,
        "rate_limit_error" => 429,
        "api_error" => 500,
        "timeout_error" => 504,
        "overloaded_error" => 529,
        _ => 500,
    }
}

impl MessagesBackend for AnthropicProvider {
    fn caps(&self) -> BackendCaps {
        BackendCaps::METERED_API
    }

    fn name(&self) -> &str {
        self.preset.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    /// The host the request goes to, derived from the very string the post uses, so this cannot
    /// name a host the request did not reach.
    fn authority(&self) -> String {
        self.url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.url)
            .split('/')
            .next()
            .unwrap_or(&self.url)
            .to_string()
    }

    fn complete(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
    ) -> Result<Completion, BackendError> {
        let eyes = self.eyes();
        let mut thinking = self.thinking_mode(req);
        let mut replay_thoughts = true;
        let mut tried_manual = thinking != Thinking::Adaptive;
        let mut tried_without_thoughts = false;
        loop {
            let policy = request::Policy {
                eyes,
                withheld: Withheld::nothing(),
                replay_thoughts,
            };
            let outcome = self.round(req, on_delta, policy, thinking);
            let Err(BackendError::Refused {
                status: 400, body, ..
            }) = &outcome
            else {
                return outcome;
            };
            // One retry each, never a ladder: a second refusal is the answer. Both are 400s, so
            // nothing of an answer has been read and the same bytes may go again.
            if !tried_manual && Self::adaptive_refused(body) {
                tried_manual = true;
                thinking = Thinking::Manual;
                continue;
            }
            if !tried_without_thoughts && replay_thoughts && Self::thoughts_refused(body) {
                tried_without_thoughts = true;
                replay_thoughts = false;
                self.note(format!(
                    "{} refused a signed thinking block (the provider said: {body}). The request \
                     went again with this conversation's thinking blocks left out, which costs \
                     reasoning continuity and is the API's own remedy; nothing else was changed.",
                    self.model
                ));
                continue;
            }
            return outcome;
        }
    }
}
