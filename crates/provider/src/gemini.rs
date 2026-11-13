//! The Google GenAI client: `generateContent`/`streamGenerateContent` with `alt=sse`.
//!
//! Every field below is mapped from a page, not from memory. The request body — `contents`,
//! `systemInstruction`, `tools`, `generationConfig` — is
//! <https://ai.google.dev/api/generate-content/stream-generate-content#request-body>; the
//! response types (`Candidate`, `finishReason`, `usageMetadata`, `Part.thoughtSignature`,
//! `Schema`) are <https://ai.google.dev/api/generate-content>; the key goes in `x-goog-api-key`
//! (<https://ai.google.dev/gemini-api/docs/function-calling>).
//!
//! What this transport refuses, and in whose words:
//!
//! - **A tool schema Gemini cannot read.** `Schema` is "a select subset of an OpenAPI 3.0
//!   schema object" (<https://ai.google.dev/api/generate-content#Resource:Schema>) with no
//!   `additionalProperties`, no `$ref`, no `oneOf`. Rather than send a request the API will
//!   reject with `Invalid JSON payload received. Unknown name …`, [`schema`] translates what it
//!   can, DROPS the annotation keys that only describe, and REFUSES the constructs that change
//!   which values are accepted — see [`DROPPED`] and [`REFUSED`].
//! - **A picture it cannot inline.** Every image is an `inlineData` blob, so a `data:` reference
//!   is the only form that goes; a URL reference is NAMED in the prompt instead, for the reason
//!   [`crate::messages::picture_fate`] gives — one bad picture refuses every turn after it.
//! - **A prompt or a candidate the model's own filter blocked.** Gemini answers 200 with no
//!   candidates and `promptFeedback.blockReason`, or with `finishReason: SAFETY`, and a caller
//!   that calls that "an empty answer" records a successful turn with nothing in it. It becomes
//!   [`BackendError::Refused`] carrying the response's own status, so the retry policy does not
//!   take the round again and the operator is told `SAFETY` rather than nothing.
//!
//! Reasoning has no `reasoning_content`. A thinking model returns `thought: true` parts, and
//! `thinkingConfig` is the only switch; the operator's `--effort` is a *level*
//! (`LOW`/`MEDIUM`/`HIGH`/`XHIGH`, <https://ai.google.dev/gemini-api/docs/thinking>), and a model
//! that does not support a level answers an error rather than ignoring it, so nothing is sent
//! until the operator asks for something.
//!
//! Thought signatures are not the reasoning row's: `ReasoningField::Signed` keeps a signature on a
//! thinking block, and `generateContent` has no thought blocks to put one on (`Completion::thoughts`
//! stays empty). "Signatures are metadata that can be attached to any part, such as living inside
//! `functionCall` parts or the final part of a response"
//! (<https://ai.google.dev/gemini-api/docs/thought-signatures?generate-content=true>), and
//! **Gemini 4 and Gemini 3 require them back on the current turn's function calls**: "the
//! **first** `functionCall` part in **each step** of the current turn **must** include its
//! `thought_signature`", validated on the current turn alone. So they are kept here, in the
//! provider, by call id — in memory, which is enough because a turn's rounds share one provider
//! and the validation does not reach back past the current turn.

use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_backend::{
    BackendCaps, BackendError, Completion, Delta, Finish, MessagesBackend, Meter, StreamFlow,
    TurnCost, TurnRequest,
};
use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem, UserPart};
use serde_json::{Map, Value, json};

use crate::keys::Credentials;
use crate::messages::{self, Eyes, Withheld};
use crate::openai::{DEFAULT_ANSWER_TIMEOUT, DEFAULT_FIRST_BYTE, Notes};
use crate::presets::Preset;

/// The `Schema` keywords that are DROPPED: they describe rather than constrain, and Gemini's
/// `Schema` has no room for them. `additionalProperties` is here because the API has no field for
/// it — an object keeps the `properties` it declares, and the keys it did not name are
/// unconstrained, which is what the tool's own argument check is for.
pub const DROPPED: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "additionalProperties",
    "default",
    "deprecated",
    "examples",
    "readOnly",
    "writeOnly",
];

/// The `Schema` keywords that are REFUSED, each with the reason, because dropping one would widen
/// or narrow what the model may send and a silent difference in a tool's contract is exactly what
/// this module exists to avoid.
pub const REFUSED: &[(&str, &str)] = &[
    (
        "$ref",
        "a $ref names a definition Gemini's Schema cannot hold",
    ),
    ("$defs", "a $defs block has no Gemini equivalent"),
    (
        "definitions",
        "a definitions block has no Gemini equivalent",
    ),
    ("oneOf", "Gemini's Schema has anyOf and no oneOf"),
    ("allOf", "Gemini's Schema has anyOf and no allOf"),
    ("not", "Gemini's Schema has no negation"),
    (
        "const",
        "Gemini's Schema has no const; a one-value enum says the same",
    ),
    (
        "patternProperties",
        "Gemini's Schema has no pattern-keyed object",
    ),
    ("if", "Gemini's Schema has no conditional"),
    ("then", "Gemini's Schema has no conditional"),
    ("else", "Gemini's Schema has no conditional"),
    ("contains", "Gemini's Schema has no contains"),
    ("uniqueItems", "Gemini's Schema has no uniqueItems"),
    ("multipleOf", "Gemini's Schema has no multipleOf"),
    ("exclusiveMinimum", "Gemini's Schema has no exclusive bound"),
    ("exclusiveMaximum", "Gemini's Schema has no exclusive bound"),
];

/// The most thought signatures one session holds. A turn is tens of rounds at most, and a turn
/// is the whole of what is validated.
pub const SIGNATURES_MAX: usize = 64;

/// The `Type` enum's words, as `Schema.type` takes them.
const TYPES: &[&str] = &[
    "TYPE_UNSPECIFIED",
    "STRING",
    "NUMBER",
    "INTEGER",
    "BOOLEAN",
    "ARRAY",
    "OBJECT",
    "NULL",
];

/// What an unanswered call is told, in the words [`crate::messages`]' filler uses.
const NO_RESULT: &str = "[no result was recorded for this call: the turn ended before it finished, \
                         or it was never run]";

/// One provider, one model, one key, over the GenAI wire.
pub struct GeminiProvider {
    preset: &'static Preset,
    model: String,
    /// Where the endpoint is built from: the preset's host and version, or the operator's
    /// override from `providers.toml`. See [`GeminiProvider::endpoint`].
    base: String,
    creds: Credentials,
    agent: ureq::Agent,
    first_byte: Duration,
    answer: Duration,
    /// Ask the model to say what it is thinking (`thinkingConfig.includeThoughts`).
    pub thinking: bool,
    /// `--effort`: the daemon's `low`/`medium`/`high`/`xhigh`, which is Gemini's thinking level.
    pub effort: Option<String>,
    /// `temperature` and friends, passed through where the GenAI body has the same name.
    pub sampling: Value,
    catalogue: Arc<crate::catalogue::Catalogue>,
    notes: Option<Notes>,
    /// The thought signatures this session's requests must hand back. See the module doc.
    signatures: Mutex<Signatures>,
}

/// The thought signatures one session has been given, by the part that carried them.
#[derive(Default)]
struct Signatures {
    /// `call id -> the call's own id at Google (if any) and its signature`.
    calls: std::collections::BTreeMap<String, Signed>,
    /// The last answer's text, hashed, and the signature its final part carried: "model responses
    /// without a function call will return a thought signature inside the last part returned".
    answer: Option<(u64, String)>,
}

#[derive(Clone, Default)]
struct Signed {
    /// The id Google gave the call, when it gave one. Only an id Google itself minted goes back
    /// in a `functionResponse.id`: the field's job is to match a call, not to name one.
    named: Option<String>,
    signature: Option<String>,
}

impl Signatures {
    fn keep(&mut self, call_id: &str, named: Option<&str>, signature: Option<&str>) {
        if named.is_none() && signature.is_none() {
            return;
        }
        let e = self.calls.entry(call_id.to_string()).or_default();
        if let Some(n) = named {
            e.named = Some(n.to_string());
        }
        if let Some(s) = signature {
            e.signature = Some(s.to_string());
        }
        while self.calls.len() > SIGNATURES_MAX {
            let Some(oldest) = self.calls.keys().next().cloned() else {
                break;
            };
            self.calls.remove(&oldest);
        }
    }

    fn of(&self, call_id: &str) -> Signed {
        self.calls.get(call_id).cloned().unwrap_or_default()
    }

    fn remember_answer(&mut self, text: &str, signature: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.answer = Some((text_key(text), signature.to_string()));
    }

    fn answer_signature(&self, text: &str) -> Option<&str> {
        match &self.answer {
            Some((k, s)) if *k == text_key(text) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// A hash of one answer's text, so the signature the model ended it with goes back on the same
/// part. FNV-1a with the length folded in, not a digest: a collision costs one misplaced signature
/// on a text part, which the API validates on function calls alone.
fn text_key(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^ (text.len() as u64)
}

impl GeminiProvider {
    pub fn new(preset: &'static Preset, model: Option<&str>, creds: Credentials) -> Self {
        let catalogue = crate::catalogue::Catalogue::load();
        let base = creds.url.clone().unwrap_or_else(|| preset.url.to_string());
        GeminiProvider {
            preset,
            model: model
                .map(str::to_string)
                .unwrap_or_else(|| preset.default_model(&catalogue)),
            base,
            creds,
            catalogue,
            agent: Self::agent(DEFAULT_FIRST_BYTE, DEFAULT_ANSWER_TIMEOUT),
            first_byte: DEFAULT_FIRST_BYTE,
            answer: DEFAULT_ANSWER_TIMEOUT,
            thinking: false,
            effort: None,
            sampling: json!({}),
            notes: None,
            signatures: Mutex::new(Signatures::default()),
        }
    }

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

    /// How long this provider may take to answer at all. A clock of its own, for the reason
    /// [`crate::openai::OpenAiProvider::with_response_timeout`] gives.
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

    /// The first-byte and answer deadlines in force.
    pub fn deadlines(&self) -> (Duration, Duration) {
        (self.first_byte.min(self.answer), self.answer)
    }

    /// The catalogue this provider reads instead of the box's, so a test's answer is about this
    /// code and not about which files the machine running it happens to have.
    pub fn with_catalogue(mut self, catalogue: Arc<crate::catalogue::Catalogue>) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// Install the way this provider tells a person something - see [`Notes`].
    pub fn with_notes(mut self, notes: Notes) -> Self {
        self.notes = Some(notes);
        self
    }

    /// The daemon's `--effort`, which Gemini spends as a thinking level.
    pub fn with_effort(mut self, effort: Option<&str>) -> Self {
        self.effort = effort.map(str::to_string);
        self
    }

    /// `…/models/{model}:streamGenerateContent?alt=sse`.
    ///
    /// The preset's URL is a BASE (`…/v1beta`) because the model name is part of the path, which
    /// no other provider's is. An operator's `providers.toml` `url` may be either that base (a
    /// proxy that mirrors the API) or a whole endpoint; one that already names `:generateContent`
    /// or `:streamGenerateContent` is used as it stands, so a key file can point at anything.
    pub fn endpoint(&self, stream: bool) -> String {
        let method = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let mut url = if self.base.contains(":streamGenerateContent")
            || self.base.contains(":generateContent")
        {
            self.base.clone()
        } else {
            format!(
                "{}/models/{}:{method}",
                self.base.trim_end_matches('/'),
                self.model
            )
        };
        if stream {
            let sep = if url.contains('?') { '&' } else { '?' };
            url = format!("{url}{sep}alt=sse");
        }
        url
    }

    /// What this model gets pictures as: a provider that already refused one, then the
    /// catalogue's `modalities.input`, and otherwise the pictures are SENT.
    fn eyes(&self) -> Eyes<'_> {
        if messages::known_text_only(self.preset.name, &self.model) {
            return Eyes::TextOnly(&self.model);
        }
        match self
            .catalogue
            .takes_images(self.preset.catalogue_id, &self.model)
        {
            Some(true) => Eyes::Sees,
            Some(false) => Eyes::TextOnly(&self.model),
            _ => Eyes::Unknown(&self.model),
        }
    }

    /// The request body, for a test to look at.
    pub fn body(&self, req: &TurnRequest<'_>) -> Result<Value, BackendError> {
        self.body_for(req, self.eyes(), Withheld::nothing())
    }

    fn body_for(
        &self,
        req: &TurnRequest<'_>,
        eyes: Eyes<'_>,
        withheld: Withheld<'_>,
    ) -> Result<Value, BackendError> {
        let declarations = tools(req.tools_json).map_err(BackendError::Malformed)?;
        let contents = {
            let sigs = self.signatures.lock().unwrap_or_else(|e| e.into_inner());
            contents(req, eyes, withheld, &sigs)
        };
        let mut body = json!({ "contents": contents });
        if let Some(s) = system_instruction(req) {
            body["systemInstruction"] = s;
        }
        if !declarations.is_empty() {
            body["tools"] = json!([{"functionDeclarations": declarations}]);
        }
        // `generationConfig` is assembled key by key so an unset switch is ABSENT rather than
        // false: `thinkingConfig` on a model that does not think is an error, not a no-op.
        let mut generation = Map::new();
        if let Some(cfg) = self.thinking_config(req.thinking.unwrap_or(self.thinking)) {
            generation.insert("thinkingConfig".into(), cfg);
        }
        if let Some(obj) = self.sampling.as_object() {
            for (k, v) in obj {
                // The knobs this transport spells its own way; anything else the operator wrote
                // for `sampling` is not this API's, and is not sent under a name it invented.
                match k.as_str() {
                    "temperature" => generation.insert("temperature".into(), v.clone()),
                    "top_p" => generation.insert("topP".into(), v.clone()),
                    "top_k" => generation.insert("topK".into(), v.clone()),
                    "presence_penalty" => generation.insert("presencePenalty".into(), v.clone()),
                    "frequency_penalty" => generation.insert("frequencyPenalty".into(), v.clone()),
                    "seed" => generation.insert("seed".into(), v.clone()),
                    "stop" | "stop_sequences" => {
                        generation.insert("stopSequences".into(), v.clone())
                    }
                    _ => None,
                };
            }
        }
        // The request's cap is a ceiling the operator's sampling cannot lift.
        if let Some(n) = req.max_output_tokens {
            let set = generation
                .get("maxOutputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            generation.insert("maxOutputTokens".into(), json!(set.min(u64::from(n))));
        }
        if !generation.is_empty() {
            body["generationConfig"] = Value::Object(generation);
        }
        Ok(body)
    }

    /// `thinkingConfig`, or `None` when this request says nothing about thinking.
    ///
    /// The level is `--effort` in the daemon's own words
    /// (<https://ai.google.dev/gemini-api/docs/thinking#thinking-levels>): `LOW`, `MEDIUM`, `HIGH`
    /// and `XHIGH` — `xhigh` on `gemini-4-hydrogen` alone, so a model without it answers an error
    /// rather than being silently downgraded here. `includeThoughts` is what makes the
    /// `thought: true` parts arrive at all.
    fn thinking_config(&self, on: bool) -> Option<Value> {
        let level = self.effort.as_deref().and_then(thinking_level);
        if !on && level.is_none() {
            return None;
        }
        let mut cfg = Map::new();
        if on {
            cfg.insert("includeThoughts".into(), Value::Bool(true));
        }
        if let Some(l) = level {
            cfg.insert("thinkingLevel".into(), Value::String(l.into()));
        }
        Some(Value::Object(cfg))
    }

    /// One attempt at the wire: build the body for `eyes`, post it, stream the answer.
    fn round(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
        eyes: Eyes<'_>,
        withheld: Withheld<'_>,
    ) -> Result<Completion, BackendError> {
        let body = self.body_for(req, eyes, withheld)?;
        let payload = body.to_string();
        let url = self.endpoint(true);
        let started = Instant::now();
        let resp = self
            .agent
            .post(&url)
            .header("x-goog-api-key", &self.creds.key)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .send(payload.as_bytes())
            .map_err(|e| BackendError::unreachable(e.to_string(), io_kind(&e)))?;
        let status = resp.status().as_u16();
        let header_retry = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if !(200..300).contains(&status) {
            let text = resp
                .into_body()
                .read_to_string()
                .unwrap_or_else(|e| format!("(unreadable body: {e})"));
            let (message, retry_after) = refusal_said(header_retry, &text);
            // What was SENT, when what was sent was refused: the shape and never the content, so
            // the next unexplained refusal is read off this record rather than guessed at from
            // documentation.
            eprintln!("  refused request shape:\n{}", shape(&body, payload.len()));
            return Err(BackendError::Refused {
                status,
                body: message,
                retry_after,
            });
        }
        let mut acc = Accumulator::default();
        let mut aborted = false;
        let mut ended = false;
        for line in BufReader::new(resp.into_body().into_reader()).lines() {
            let line = line.map_err(|e| {
                BackendError::unreachable(format!("mid-stream: {e}"), Some(e.kind()))
            })?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                ended = true;
                break;
            }
            let chunk: Value = serde_json::from_str(data)
                .map_err(|e| BackendError::Malformed(format!("{e} in chunk {data}")))?;
            // The headers carried a 200 before this byte, so a `{"error":…}` chunk is the only
            // place a mid-stream refusal can arrive.
            if let Some(refusal) = in_band_refusal(&chunk, status, header_retry) {
                return Err(refusal);
            }
            for d in acc.absorb(&chunk) {
                if on_delta(&d) == StreamFlow::Stop {
                    aborted = true;
                    break;
                }
            }
            if aborted {
                break;
            }
        }
        if aborted {
            return Err(BackendError::Aborted);
        }
        if !ended && !acc.settled() {
            // A half answer is not an answer: `finishReason` is set on the last chunk of a
            // candidate, so its absence is a socket that closed early.
            return Err(BackendError::unreachable(
                format!(
                    "the stream closed without a finishReason after {} character(s) of answer \
                     and {} tool call(s), so what arrived is a fragment",
                    acc.text.chars().count() + acc.reasoning.chars().count(),
                    acc.calls.len()
                ),
                None,
            ));
        }
        // The signatures this round earned, for the next round of THIS turn to hand back.
        {
            let mut sigs = self.signatures.lock().unwrap_or_else(|e| e.into_inner());
            for (id, named, sig) in &acc.signed {
                sigs.keep(id, named.as_deref(), sig.as_deref());
            }
            if let Some(sig) = acc.answer_signature.as_deref() {
                sigs.remember_answer(&acc.text, sig);
            }
        }
        let wall_ms = started.elapsed().as_millis() as u64;
        acc.finish(
            status,
            &self.model,
            &self.creds,
            wall_ms,
            self.preset,
            &self.catalogue,
        )
    }

    /// A fact the operator must read: the daemon's log first, because a session may have no head
    /// attached, then whatever the caller installed as its notes.
    fn say(&self, said: String) {
        eprintln!("  {said}");
        if let Some(notes) = &self.notes {
            notes(said);
        }
    }
}

impl MessagesBackend for GeminiProvider {
    fn caps(&self) -> BackendCaps {
        BackendCaps::METERED_API
    }

    fn name(&self) -> &str {
        self.preset.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    /// The host the request goes to, derived from the URL the post uses, so this cannot name a
    /// host the request did not reach.
    fn authority(&self) -> String {
        let url = self.endpoint(true);
        url.split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&url)
            .split('/')
            .next()
            .unwrap_or(&url)
            .to_string()
    }

    fn complete(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
    ) -> Result<Completion, BackendError> {
        let eyes = self.eyes();
        let first = self.round(req, on_delta, eyes, Withheld::nothing());
        // The two picture refusals are read off the wire, never assumed: the status is 400 and the
        // request ACTUALLY carried a picture. A model that takes text only is marked for the
        // session; bytes it will not take are marked against the bytes.
        let Err(BackendError::Refused {
            status: 400,
            body: why,
            ..
        }) = &first
        else {
            return first;
        };
        let carried = messages::carries_pictures(req.items.iter().chain(req.tail.iter()));
        if !eyes.takes_images() || !carried {
            return first;
        }
        // One retry, never a ladder: a second refusal is the answer.
        if messages::text_only_refusal(why) {
            if messages::mark_text_only(self.preset.name, &self.model) {
                self.say(format!(
                    "{} refused an image; images are now sent to it as placeholders (the provider \
                     said: {why}). The request went again without them, and this daemon will not \
                     send this model another picture.",
                    self.model
                ));
            }
            return self.round(
                req,
                on_delta,
                Eyes::TextOnly(&self.model),
                Withheld::nothing(),
            );
        }
        if !messages::picture_refusal(why) {
            return first;
        }
        let pictures: Vec<String> = messages::pictures(req.items.iter().chain(req.tail.iter()))
            .into_iter()
            .map(String::from)
            .collect();
        if pictures.is_empty() {
            return first;
        }
        let trying: Vec<(String, String)> = pictures
            .iter()
            .map(|d| (messages::picture_key(d), why.clone()))
            .collect();
        let second = self.round(req, on_delta, eyes, Withheld::trying(&trying));
        match &second {
            Ok(_) => {
                let mut fresh = 0;
                for d in &pictures {
                    if messages::mark_bad_picture(d, why) {
                        fresh += 1;
                    }
                }
                self.say(format!(
                    "{} refused {} picture(s) (the provider said: {why}). The request went again \
                     with them named instead of sent and was answered; this daemon will not send \
                     those bytes again{}. The pictures themselves are unchanged on disk.",
                    self.model,
                    pictures.len(),
                    if fresh == 0 {
                        " — they were already known"
                    } else {
                        ""
                    }
                ));
            }
            Err(BackendError::Refused { body, .. }) => self.say(format!(
                "{} refused a picture (the provider said: {why}), and refused the request again \
                 with every picture in it withheld (the provider said: {body}). Nothing has been \
                 recorded against the pictures: the second refusal may be about something else, \
                 and the next turn will try them again.",
                self.model
            )),
            Err(e) => self.say(format!(
                "{} refused a picture (the provider said: {why}), and the retry without pictures \
                 failed before the provider answered: {e}",
                self.model
            )),
        }
        second
    }
}

/// The `Type` word for a JSON Schema type, or `None` when the vocabulary is not Gemini's.
fn gemini_type(t: &str) -> Option<&'static str> {
    let up = t.to_ascii_uppercase();
    TYPES.iter().copied().find(|w| *w == up)
}

/// `--effort` as Gemini's thinking level. `max` is the launcher's word for the top rung, and an
/// unknown word says nothing rather than guessing a level for the operator.
fn thinking_level(effort: &str) -> Option<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "minimal" => Some("MINIMAL"),
        "low" => Some("LOW"),
        "medium" => Some("MEDIUM"),
        "high" => Some("HIGH"),
        "xhigh" | "max" => Some("XHIGH"),
        _ => None,
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

/// The provider's refusal, in its own words plus the one machine-readable word it carries.
///
/// ```text
/// {"error": {"code": 429, "message": "You exceeded your current quota…",
///            "status": "RESOURCE_EXHAUSTED",
///            "details": [{"@type": "…/google.rpc.RetryInfo", "retryDelay": "17s"}]}}
/// ```
///
/// The status word goes into the sentence for the reason `openai.rs` keeps a code: it is the only
/// word a caller can match on, and a `429` is `RESOURCE_EXHAUSTED` here
/// (<https://ai.google.dev/gemini-api/docs/rate-limits>). `RetryInfo.retryDelay` is the provider's
/// own instant when no `Retry-After` header carried one.
fn refusal_said(header_retry: Option<u64>, text: &str) -> (String, Option<u64>) {
    let parsed: Option<Value> = serde_json::from_str(text).ok();
    let err = parsed.as_ref().and_then(|v| v.get("error"));
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| text.trim().to_string());
    let word = err.and_then(|e| e.get("status")).and_then(Value::as_str);
    let message = match word {
        Some(w) if !message.contains(w) => format!("{message} ({w})"),
        _ => message,
    };
    let retry_after = header_retry.or_else(|| err.and_then(retry_delay));
    (message, retry_after)
}

/// `details[].retryDelay` (`"17s"`, `"1.5s"`) as whole seconds, which is the only form a caller
/// may sleep on.
fn retry_delay(err: &Value) -> Option<u64> {
    let details = err.get("details")?.as_array()?;
    for d in details {
        if let Some(text) = d.get("retryDelay").and_then(Value::as_str)
            && let Ok(secs) = text.trim().trim_end_matches('s').parse::<f64>()
        {
            return Some(secs.ceil() as u64);
        }
    }
    None
}

/// The refusal a provider sends INSIDE its stream, which has no status of its own because the
/// headers went out with `200`. `None` for a chunk that is not an error.
fn in_band_refusal(chunk: &Value, status: u16, header_retry: Option<u64>) -> Option<BackendError> {
    let err = chunk.get("error")?;
    let (message, retry_after) = refusal_said(header_retry, &err.to_string());
    let code = err
        .get("code")
        .and_then(Value::as_u64)
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| (400..600).contains(n))
        .unwrap_or(status);
    Some(BackendError::Refused {
        status: code,
        body: message,
        retry_after,
    })
}

/// The SHAPE of a request that was refused: how many contents, which role each is, and what kinds
/// of part it holds. Never the text, for the reason `openai.rs`'s `shape` gives — a log that
/// quoted the conversation would be a second copy of the transcript.
fn shape(body: &Value, bytes: usize) -> String {
    let n_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .and_then(|t| t.first())
        .and_then(|t| t.get("functionDeclarations"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut out = format!(
        "    {bytes} bytes, tools={n_tools} thinking={} top-level-keys=[{}]",
        body.get("generationConfig")
            .and_then(|g| g.get("thinkingConfig"))
            .is_some(),
        body.as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
    );
    let none: Vec<Value> = Vec::new();
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .unwrap_or(&none);
    out.push_str(&format!("\n    {} content(s):", contents.len()));
    for (i, c) in contents.iter().enumerate() {
        let parts = c.get("parts").and_then(Value::as_array).unwrap_or(&none);
        let kinds: Vec<String> = parts.iter().map(one_part).collect();
        out.push_str(&format!(
            "\n      [{i}] {:<5} parts={} [{}]",
            c.get("role").and_then(Value::as_str).unwrap_or("?"),
            parts.len(),
            kinds.join(" ")
        ));
    }
    out
}

/// One part of a [`shape`] dump: its kind and, for text, a length. `absent` and present differ,
/// and the difference is the whole point of the dump.
fn one_part(p: &Value) -> String {
    if let Some(t) = p.get("text").and_then(Value::as_str) {
        let thought = if p.get("thought").and_then(Value::as_bool) == Some(true) {
            "thought"
        } else {
            "text"
        };
        return format!("{thought}={}ch", t.chars().count());
    }
    for kind in ["functionCall", "functionResponse", "inlineData"] {
        if p.get(kind).is_some() {
            let sig = p.get("thoughtSignature").is_some();
            return format!("{kind}{}", if sig { "+sig" } else { "" });
        }
    }
    "?".to_string()
}

/// `systemInstruction`: the caller's system string, and any System item the transcript LEADS
/// with, joined. Gemini has one instruction field and no system ROLE, so a system item that
/// arrives mid-history becomes a user turn instead — see [`contents`].
fn system_instruction(req: &TurnRequest<'_>) -> Option<Value> {
    let mut said = String::new();
    if !req.system.trim().is_empty() {
        said.push_str(req.system);
    }
    if let Some(TranscriptItem::System { text, .. }) = req.items.first()
        && !text.trim().is_empty()
    {
        if !said.is_empty() {
            said.push_str("\n\n");
        }
        said.push_str(text);
    }
    if said.trim().is_empty() {
        return None;
    }
    Some(json!({"parts": [{"text": said}]}))
}

/// The `contents` array: roles `user` and `model`, one `Content` per item.
///
/// The walk is the pairing machine [`crate::messages`] documents, in this protocol's shapes: a
/// model turn's `functionCall` parts are answered by `functionResponse` parts, an unanswered call
/// is filled rather than sent to be refused, an orphan result is dropped unless the OPERATOR ran
/// it, and a row that lands between a call and its result is held until the results do.
fn contents(
    req: &TurnRequest<'_>,
    eyes: Eyes<'_>,
    withheld: Withheld<'_>,
    sigs: &Signatures,
) -> Vec<Value> {
    let mut out = Contents::default();
    // The transcript's leading System item is the system instruction, not a turn.
    let items = match req.items.first() {
        Some(TranscriptItem::System { .. }) => &req.items[1..],
        _ => req.items,
    };
    for item in items.iter().chain(req.tail.iter()) {
        match item {
            TranscriptItem::System { text, .. } => {
                // No system role mid-history: the words are the harness's, said as a user turn.
                out.push_turn(json!({"role": "user", "parts": [{"text": text}]}));
            }
            TranscriptItem::Notice { text, .. } => {
                out.push_turn(json!({"role": "user", "parts": [{"text": text}]}));
            }
            TranscriptItem::User { parts, .. } => {
                let mut said: Vec<Value> = Vec::new();
                let mut images = 0usize;
                for p in parts {
                    match p {
                        UserPart::Text { text } => said.push(json!({"text": text})),
                        UserPart::Image {
                            media_type,
                            data_ref,
                        } => {
                            images += 1;
                            match picture_fate_part(data_ref, eyes, withheld) {
                                Ok(part) => said.push(part),
                                Err(why) => said.push(json!({"text": messages::not_sent_said(
                                    images,
                                    media_type,
                                    messages::data_ref_bytes(data_ref),
                                    &why,
                                )})),
                            }
                        }
                        UserPart::FileRef { path, .. } => said.push(json!({"text": path})),
                    }
                }
                if !said.is_empty() {
                    out.push_turn(json!({"role": "user", "parts": said}));
                }
            }
            // A thought is not replayed: `generateContent` has no thought blocks, and the
            // signature that carries the reasoning forward travels on its own part.
            TranscriptItem::Reasoning { .. } => {}
            TranscriptItem::SegmentMark { .. } | TranscriptItem::Offer { .. } => {}
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                let mut parts: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    let mut part = json!({"text": text});
                    if let Some(sig) = sigs.answer_signature(text) {
                        part["thoughtSignature"] = Value::String(sig.to_string());
                    }
                    parts.push(part);
                }
                let mut owed = Vec::new();
                for c in tool_calls {
                    let mut part = json!({
                        "functionCall": {"name": c.name, "args": args_of(&c.arguments)}
                    });
                    let signed = sigs.of(&c.id);
                    // "You must return this signature in the exact part where it was received":
                    // it went on the FIRST `functionCall` part of Google's reply, and it goes back
                    // on the same call here.
                    if let Some(sig) = signed.signature {
                        part["thoughtSignature"] = Value::String(sig);
                    }
                    if let Some(named) = signed.named {
                        part["functionCall"]["id"] = Value::String(named);
                    }
                    parts.push(part);
                    owed.push((c.id.clone(), c.name.clone()));
                }
                if !parts.is_empty() {
                    out.push_model(json!({"role": "model", "parts": parts}), owed);
                }
            }
            TranscriptItem::ToolResult {
                call_id,
                name,
                outcome,
                payload,
                media,
                origin,
                ..
            } => {
                if !out.answers(call_id) {
                    // An orphan has no model turn above it naming this call, and Gemini would be
                    // handed a response to a call it never made. The operator's own runs become
                    // the person's turn, which is what they are.
                    if matches!(
                        origin,
                        Some(letibot_transcript::CallOrigin::Operator { .. })
                    ) {
                        out.push_turn(json!({
                            "role": "user",
                            "parts": [{"text": payload}],
                        }));
                    }
                    continue;
                }
                let mut response = response_of(outcome, payload);
                let mut part = json!({"functionResponse": {"name": name}});
                if let Some(named) = sigs.of(call_id).named {
                    part["functionResponse"]["id"] = Value::String(named);
                }
                // Bytes the call read belong INSIDE the response: "include multimodal content in
                // the function response parts"
                // (<https://ai.google.dev/gemini-api/docs/function-calling>).
                if let Some(m) = media {
                    match picture_fate_part(&m.data_ref, eyes, withheld) {
                        Ok(mut p) => {
                            if let Some(blob) = p.get_mut("inlineData") {
                                blob["displayName"] = Value::String(name.clone());
                            }
                            part["functionResponse"]["parts"] = json!([p]);
                        }
                        Err(why) => {
                            if let Some(obj) = response.as_object_mut() {
                                obj.insert(
                                    "media".into(),
                                    json!(messages::not_sent_said(1, &m.mime, Some(m.bytes), &why)),
                                );
                            }
                        }
                    }
                }
                part["functionResponse"]["response"] = response;
                out.push_result(json!({"role": "user", "parts": [part]}));
            }
        }
    }
    out.finish()
}

/// The pairing machine behind [`contents`], kept apart from the walk so the order rules live in
/// one place.
#[derive(Default)]
struct Contents {
    out: Vec<Value>,
    /// `(call id, call name)` of the model turn being answered, in the order it asked.
    owed: Vec<(String, String)>,
    /// Rows recorded while calls were still running: a room note, a monitor firing. They wait
    /// here until the results land, so a note between a call and its result never orphans it.
    held: Vec<Value>,
}

impl Contents {
    /// A content of the conversation's own: while calls are owed it waits, because a row that
    /// lands between a call and its result must not separate them.
    fn push_turn(&mut self, c: Value) {
        if self.owed.is_empty() {
            self.out.push(c);
        } else {
            self.held.push(c);
        }
    }

    /// A content answering calls: it belongs to the answering window, so it goes out at once, and
    /// the rows held during the window follow it once nothing is owed.
    fn push_result(&mut self, c: Value) {
        self.out.push(c);
        if self.owed.is_empty() {
            self.out.append(&mut self.held);
        }
    }

    /// A model turn. Whatever the previous one still owes is owed for ever — the next assistant
    /// message closes the answering window — so it is filled here, then the held rows, then this.
    fn push_model(&mut self, c: Value, owed: Vec<(String, String)>) {
        for (_, name) in std::mem::take(&mut self.owed) {
            self.out.push(filler(&name));
        }
        self.out.append(&mut self.held);
        self.out.push(c);
        self.owed = owed;
    }

    /// Whether this id is one the model actually asked for, taking it off the owed list.
    fn answers(&mut self, call_id: &str) -> bool {
        if let Some(at) = self.owed.iter().position(|(id, _)| id == call_id) {
            self.owed.remove(at);
            return true;
        }
        false
    }

    fn finish(mut self) -> Vec<Value> {
        // A transcript can simply end on an unanswered call — an interrupted turn does exactly
        // that — and Gemini counts it the same way.
        for (_, name) in std::mem::take(&mut self.owed) {
            self.out.push(filler(&name));
        }
        self.out.append(&mut self.held);
        self.out
    }
}

/// The function response for a call nothing recorded: an answer, and one that says so, rather
/// than a fabricated success or a request the API refuses.
fn filler(name: &str) -> Value {
    json!({
        "role": "user",
        "parts": [{"functionResponse": {"name": name, "response": {"result": NO_RESULT}}}],
    })
}

/// The `response` object of a `functionResponse`: `{"result": …}`, or `{"error": …}` for the
/// outcomes that mean the call did not answer — the key the API documents: "if the function call
/// failed to execute, the response can have an 'error' key to return error details"
/// (<https://ai.google.dev/api/generate-content#Resource:FunctionResponse>).
fn response_of(outcome: &ToolOutcome, payload: &str) -> Value {
    match outcome {
        ToolOutcome::Ok | ToolOutcome::Abstained { .. } | ToolOutcome::Backgrounded { .. } => {
            json!({"result": payload})
        }
        ToolOutcome::Failed { .. }
        | ToolOutcome::Denied { .. }
        | ToolOutcome::Timeout
        | ToolOutcome::NotRun { .. } => json!({"error": payload}),
    }
}

/// A picture as an `inlineData` part, or why it cannot be one. Every image Gemini is given is
/// inline bytes: `fileData` names a `fileUri` this harness never creates, so a reference that is
/// not `data:<mime>;base64,…` is NAMED in the prompt instead.
fn picture_fate_part(
    data_ref: &str,
    eyes: Eyes<'_>,
    withheld: Withheld<'_>,
) -> Result<Value, String> {
    let blob = inline_blob(data_ref);
    // The model first — a picture it cannot see is the model's fact, whatever the bytes are — and
    // then this transport's own limit, which is not a verdict on bytes a reference never had.
    if eyes.takes_images() && blob.is_none() {
        return Err(NOT_INLINE.into());
    }
    if let Some(why) = messages::picture_fate(data_ref, eyes, withheld) {
        return Err(why);
    }
    let (mime, data) = blob.ok_or_else(|| NOT_INLINE.to_string())?;
    Ok(json!({"inlineData": {"mimeType": mime, "data": data}}))
}

/// Why a picture is withheld when the reference is not inline bytes at all.
const NOT_INLINE: &str = "only inline image bytes are sent, and this reference is not one";

/// The `(mimeType, base64)` a `data:` reference carries, or `None` when it is not one. The prefix
/// is stripped because `inlineData.data` is the base64 itself: sent whole, it would be decoded as
/// image bytes and the picture would arrive corrupt.
fn inline_blob(data_ref: &str) -> Option<(&str, &str)> {
    let (head, _) = data_ref.split_once(";base64,")?;
    let mime = head.strip_prefix("data:")?;
    Some((mime, letibot_transcript::media::wire_payload(data_ref)))
}

/// A tool call's arguments as the `args` OBJECT: Gemini takes an object, and a transcript holds
/// the raw text the model emitted. Text that is not a JSON object cannot be sent as one, so the
/// call goes with no arguments — its result row says it was cut off, which is the truth the `{}`
/// is not claiming.
fn args_of(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(arguments) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

/// Tool schemas as `functionDeclarations`. The harness hands over its dialect's rendering —
/// `{"type":"function","function":{name,description,parameters}}` or the bare form — and anything
/// else is refused rather than sent as a string the provider would 400 on.
pub fn tools(tools_json: &[String]) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(tools_json.len());
    for t in tools_json {
        let v: Value =
            serde_json::from_str(t).map_err(|e| format!("a tool schema is not JSON: {e}"))?;
        let f = match v.get("function") {
            Some(f) => f.clone(),
            None if v.get("name").is_some() => v.clone(),
            None => {
                return Err(format!(
                    "a tool schema has neither `function` nor `name`: {}",
                    t.chars().take(80).collect::<String>()
                ));
            }
        };
        let name = f
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "a tool schema names no function".to_string())?;
        // `description` is required by the reference; a name is a poor one but it is not a lie,
        // and a declaration without the field fails the whole request.
        let description = f
            .get("description")
            .and_then(Value::as_str)
            .filter(|d| !d.trim().is_empty())
            .unwrap_or(name);
        let mut decl = Map::new();
        decl.insert("name".into(), Value::String(name.to_string()));
        decl.insert("description".into(), Value::String(description.to_string()));
        if let Some(params) = f.get("parameters")
            && !params.is_null()
        {
            decl.insert(
                "parameters".into(),
                schema(params, &format!("{name}.parameters"))?,
            );
        }
        out.push(Value::Object(decl));
    }
    Ok(out)
}

/// One JSON Schema as Gemini's `Schema`, which "represents a select subset of an OpenAPI 3.0
/// schema object".
///
/// Three things happen to a keyword: it is CONVERTED when the name or the spelling differs
/// (`type: ["boolean","integer"]` becomes `anyOf`; `minItems: 5` becomes `"5"`, because the
/// reference types those as strings), DROPPED when it only describes ([`DROPPED`]), and REFUSED
/// when it changes which values are accepted and has no equivalent ([`REFUSED`]). A refusal names
/// the parameter and the keyword, because the fix is in the tool's own schema.
pub fn schema(v: &Value, at: &str) -> Result<Value, String> {
    if !v.is_object() {
        return Err(format!(
            "`{at}` is not a schema object: {}",
            v.to_string().chars().take(60).collect::<String>()
        ));
    }
    for (key, why) in REFUSED {
        if v.get(*key).is_some() {
            return Err(format!("`{at}` carries `{key}`: {why}"));
        }
    }
    let mut out = Map::new();
    match v.get("type") {
        // A union of types is `anyOf` here: `Schema.type` is ONE word.
        Some(Value::Array(words)) => {
            if v.get("enum").is_some() {
                return Err(format!(
                    "`{at}` has an enum under a union type, and Gemini's enum belongs to one type"
                ));
            }
            let mut branches = Vec::with_capacity(words.len());
            for w in words {
                let word = w
                    .as_str()
                    .and_then(gemini_type)
                    .ok_or_else(|| format!("`{at}.type` names `{w}`, which is not a JSON type"))?;
                branches.push(json!({"type": word}));
            }
            out.insert("anyOf".into(), Value::Array(branches));
        }
        Some(Value::String(t)) => {
            let word = gemini_type(t).ok_or_else(|| {
                format!(
                    "`{at}.type` is `{t}`, which is not one of Gemini's types ({}); a nullable \
                     value is `nullable`, not a `null` type",
                    TYPES.join(", ")
                )
            })?;
            out.insert("type".into(), Value::String(word.into()));
        }
        // No type at all: `anyOf` and `properties` are their own declaration, and a node with
        // neither is a parameter nobody could send.
        None => {
            if v.get("anyOf").is_none() && v.get("properties").is_none() && v.get("items").is_none()
            {
                return Err(format!(
                    "`{at}` has no `type`, no `anyOf` and no `properties`, so Gemini has nothing \
                     to read"
                ));
            }
        }
        Some(other) => {
            return Err(format!(
                "`{at}.type` is {other}, and a type is a word or a list of them"
            ));
        }
    }
    for key in ["format", "title", "description", "nullable"] {
        if let Some(x) = v.get(key) {
            out.insert(key.into(), x.clone());
        }
    }
    if let Some(e) = v.get("enum") {
        let words: Vec<Value> = e
            .as_array()
            .ok_or_else(|| format!("`{at}.enum` is not an array"))?
            .iter()
            .map(|w| match w {
                // `enum` is "possible values of the element of Type.STRING": a number here is a
                // schema the API would reject, so it is refused by name.
                Value::String(s) => Ok(Value::String(s.clone())),
                other => Err(format!(
                    "`{at}.enum` holds {other}, and Gemini's enum is a list of strings"
                )),
            })
            .collect::<Result<_, _>>()?;
        out.insert("enum".into(), Value::Array(words));
    }
    if let Some(items) = v.get("items") {
        out.insert("items".into(), schema(items, &format!("{at}.items"))?);
    }
    if let Some(props) = v.get("properties") {
        let mut converted = Map::new();
        let obj = props
            .as_object()
            .ok_or_else(|| format!("`{at}.properties` is not an object"))?;
        for (name, sub) in obj {
            converted.insert(
                name.clone(),
                schema(sub, &format!("{at}.properties.{name}"))?,
            );
        }
        out.insert("properties".into(), Value::Object(converted));
    }
    if let Some(req) = v.get("required") {
        out.insert("required".into(), req.clone());
    }
    if let Some(list) = v.get("anyOf") {
        let mut branches = Vec::new();
        for (i, b) in list
            .as_array()
            .ok_or_else(|| format!("`{at}.anyOf` is not an array"))?
            .iter()
            .enumerate()
        {
            branches.push(schema(b, &format!("{at}.anyOf[{i}]"))?);
        }
        out.insert("anyOf".into(), Value::Array(branches));
    }
    for key in ["propertyOrdering", "pattern"] {
        if let Some(x) = v.get(key) {
            out.insert(key.into(), x.clone());
        }
    }
    for key in ["minimum", "maximum"] {
        if let Some(n) = v.get(key) {
            out.insert(key.into(), n.clone());
        }
    }
    for key in [
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
        "minLength",
        "maxLength",
    ] {
        if let Some(n) = v.get(key) {
            // "Minimum number of the elements for Type.ARRAY", typed `string` in the reference:
            // int64 on the wire, which protobuf's JSON form spells as a quoted integer.
            let text = match n {
                Value::Number(number) => number.to_string(),
                Value::String(s) => s.clone(),
                other => {
                    return Err(format!(
                        "`{at}.{key}` is {other}, and it must be an integer"
                    ));
                }
            };
            out.insert(key.into(), Value::String(text));
        }
    }
    Ok(Value::Object(out))
}

/// What the deltas build up, and the last chunk's `usageMetadata`.
#[derive(Default)]
struct Accumulator {
    text: String,
    reasoning: String,
    calls: Vec<(String, String, String)>, // (id, name, arguments) in arrival order
    /// `(call id, the id Google gave it, its thought signature)`.
    signed: Vec<(String, Option<String>, Option<String>)>,
    /// The signature the answer's final part carried.
    answer_signature: Option<String>,
    finish: Option<Finish>,
    /// Why the model stopped, in ITS words, when the reason is not an ending. `None` for
    /// `STOP` and `MAX_TOKENS`.
    blocked: Option<String>,
    /// `promptFeedback.blockReason`: the prompt itself was refused, and no candidates follow.
    prompt_block: Option<String>,
    usage: Option<Value>,
    /// The model the provider says served the turn (`modelVersion`), which is what gets billed.
    model: Option<String>,
}

impl Accumulator {
    /// Whether the stream said how it ended, which is the whole of "this answer is complete".
    fn settled(&self) -> bool {
        self.finish.is_some() || self.prompt_block.is_some()
    }

    /// One SSE chunk → the deltas it carries, in order.
    fn absorb(&mut self, chunk: &Value) -> Vec<Delta> {
        let mut out = Vec::new();
        if let Some(m) = chunk.get("modelVersion").and_then(Value::as_str)
            && !m.is_empty()
        {
            self.model = Some(m.to_string());
        }
        if let Some(u) = chunk.get("usageMetadata").filter(|u| !u.is_null()) {
            self.usage = Some(u.clone());
        }
        if let Some(reason) = chunk
            .get("promptFeedback")
            .and_then(|f| f.get("blockReason"))
            .and_then(Value::as_str)
        {
            self.prompt_block = Some(reason.to_string());
        }
        let Some(candidates) = chunk.get("candidates").and_then(Value::as_array) else {
            return out;
        };
        for candidate in candidates {
            if let Some(fr) = candidate.get("finishReason").and_then(Value::as_str) {
                self.finish = Some(finish_of(fr));
                self.blocked = blocked_by(candidate, fr);
            }
            let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for part in parts {
                let signature = part.get("thoughtSignature").and_then(Value::as_str);
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    if t.is_empty() {
                        continue;
                    }
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        self.reasoning.push_str(t);
                        out.push(Delta::Reasoning(t.to_string()));
                    } else {
                        self.text.push_str(t);
                        if let Some(sig) = signature {
                            self.answer_signature = Some(sig.to_string());
                        }
                        out.push(Delta::Text(t.to_string()));
                    }
                    continue;
                }
                let Some(call) = part.get("functionCall") else {
                    continue;
                };
                let name = call
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default();
                // A call Google named keeps that id: it is what the next request must echo. One
                // it did not gets a minted id, so no two turns share `call_0`.
                let named = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let id = named
                    .map(str::to_string)
                    .unwrap_or_else(letibot_transcript::mint_call_id);
                let arguments = call
                    .get("args")
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                let index = self.calls.len();
                self.calls
                    .push((id.clone(), name.clone(), arguments.clone()));
                self.signed.push((
                    id.clone(),
                    named.map(str::to_string),
                    signature.map(String::from),
                ));
                out.push(Delta::ToolCall {
                    index,
                    id: Some(id),
                    name: Some(name),
                    arguments,
                });
            }
        }
        out
    }

    fn finish(
        mut self,
        status: u16,
        model: &str,
        creds: &Credentials,
        wall_ms: u64,
        preset: &'static crate::presets::Preset,
        catalogue: &crate::catalogue::Catalogue,
    ) -> Result<Completion, BackendError> {
        let calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments,
            })
            .collect();
        // A prompt the model's filter refused, and a candidate it stopped for a reason that is
        // not an ending with nothing to show for it: the reason IS the answer, and recording it
        // as a successful empty turn is the one thing not to do.
        if let Some(reason) = self.prompt_block.take() {
            return Err(BackendError::Refused {
                status,
                body: format!("the prompt was blocked: {reason}"),
                retry_after: None,
            });
        }
        if let Some(reason) = self.blocked.take()
            && self.text.trim().is_empty()
            && calls.is_empty()
        {
            return Err(BackendError::Refused {
                status,
                body: format!("the model stopped and produced nothing: {reason}"),
                retry_after: None,
            });
        }
        let u = self.usage.as_ref();
        let get = |k: &str| {
            u.and_then(|u| u.get(k))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let prompt_tokens = get("promptTokenCount");
        // Thoughts are billed as output ("pricing is the sum of output tokens and thinking
        // tokens") and `candidatesTokenCount` excludes them, so the generated count is both:
        // costing only the candidates would report a thinking turn as nearly free.
        let thoughts = get("thoughtsTokenCount");
        let generated_tokens = get("candidatesTokenCount").saturating_add(thoughts);
        let cached_tokens = get("cachedContentTokenCount");
        // The operator's file first, then the catalogue, and the key is the SERVER's name: asking
        // for one model can be answered by another, so pricing the request bills the wrong rates.
        let billed = self.model.as_deref().unwrap_or(model);
        let micros_usd = creds
            .prices
            .get(billed)
            .copied()
            .or_else(|| {
                catalogue
                    .model(preset.catalogue_id, billed)
                    .and_then(|m| m.prices)
            })
            .map(|p| p.micros(prompt_tokens, cached_tokens, generated_tokens));
        Ok(Completion {
            text: self.text,
            reasoning: self.reasoning,
            tool_calls: calls,
            finish: self.finish.unwrap_or(Finish::Stop),
            cost: TurnCost {
                meter: Meter::Metered,
                prompt_tokens,
                cached_tokens,
                generated_tokens,
                wall_ms,
                micros_usd,
            },
            raw_usage: u.map(|u| u.to_string()),
            thoughts: Vec::new(),
        })
    }
}

/// `finishReason` as letibot's [`Finish`]. `STOP` is an ending and `MAX_TOKENS` is a length; the
/// rest are carried verbatim, because a head that drew "eos" for `SAFETY` told the operator the
/// model finished its thought.
fn finish_of(word: &str) -> Finish {
    match word {
        "STOP" | "FINISH_REASON_UNSPECIFIED" => Finish::Stop,
        "MAX_TOKENS" => Finish::Length,
        other => Finish::Other(other.to_string()),
    }
}

/// Whether this finish reason means "not this answer", and the sentence that says why: `None` for
/// the endings, and `Some` for everything the reference lists that is a refusal —
/// `SAFETY`, `RECITATION`, `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII`, `LANGUAGE`, `MALFORMED_FUNCTION_CALL`,
/// `MISSING_THOUGHT_SIGNATURE`, `UNEXPECTED_TOOL_CALL`, `TOO_MANY_TOOL_CALLS`,
/// `IMAGE_SAFETY` and the rest.
fn blocked_by(candidate: &Value, word: &str) -> Option<String> {
    if matches!(word, "STOP" | "MAX_TOKENS" | "FINISH_REASON_UNSPECIFIED") {
        return None;
    }
    let mut said = format!("finishReason={word}");
    if let Some(message) = candidate.get("finishMessage").and_then(Value::as_str)
        && !message.trim().is_empty()
    {
        said.push_str(&format!(" ({message})"));
    }
    // The categories that fired, and how likely each was: `SAFETY` alone says nothing about
    // which one, and the operator's remedy differs.
    if let Some(ratings) = candidate.get("safetyRatings").and_then(Value::as_array) {
        let fired: Vec<String> = ratings
            .iter()
            .filter(|r| r.get("blocked").and_then(Value::as_bool) == Some(true))
            .filter_map(|r| {
                Some(format!(
                    "{}={}",
                    r.get("category")?.as_str()?,
                    r.get("probability")?.as_str()?
                ))
            })
            .collect();
        if !fired.is_empty() {
            said.push_str(&format!(" [{}]", fired.join(" ")));
        }
    }
    Some(said)
}

#[cfg(test)]
mod tests;
