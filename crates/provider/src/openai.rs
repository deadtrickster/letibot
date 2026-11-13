//! The OpenAI-shaped chat-completions client, streaming.

use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use letibot_backend::{
    BackendCaps, BackendError, Completion, Delta, Finish, MessagesBackend, Meter, StreamFlow,
    TurnCost, TurnRequest,
};
use letibot_transcript::ToolCall;
use serde_json::{Value, json};

use crate::keys::Credentials;
use crate::presets::Preset;

/// How long a provider may take to send its first byte: SSE headers go out with the first chunk,
/// so a provider that says nothing at all is caught here and not at the answer's budget.
pub const DEFAULT_FIRST_BYTE: Duration = Duration::from_secs(60);

/// The budget for the whole stream once bytes are arriving: a long answer is progress, not a hang.
pub const DEFAULT_ANSWER_TIMEOUT: Duration = Duration::from_secs(600);

/// One provider, one model, one key.
pub struct OpenAiProvider {
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
    /// `temperature` and friends, passed through verbatim.
    pub sampling: Value,
    /// The catalogue this provider looks a model up in: the box's by default, and a
    /// caller's own when a test must not read the machine's files.
    catalogue: std::sync::Arc<crate::catalogue::Catalogue>,
    /// Where a fact the operator must read is said: the daemon's hub, one crate up. A closure
    /// rather than a hub, because this crate stays below the session log and who hears it is the
    /// caller's business.
    notes: Option<Notes>,
    /// `prompt_cache_key`, for Kimi's implicit cache: one provider instance is one session, and no
    /// session id reaches this crate, so the instance's own name is the key. `None` elsewhere.
    cache_key: Option<String>,
}

/// Where a provider says something a person has to read. `Arc<dyn Fn>` because the
/// provider is `Send + Sync` and cloned into a `Box<dyn MessagesBackend>`.
pub type Notes = std::sync::Arc<dyn Fn(String) + Send + Sync>;

impl OpenAiProvider {
    pub fn new(preset: &'static Preset, model: Option<&str>, creds: Credentials) -> Self {
        let url = creds.url.clone().unwrap_or_else(|| preset.url.to_string());
        let catalogue = crate::catalogue::Catalogue::load();
        OpenAiProvider {
            preset,
            // The catalogue's pick when the operator named no model, else this
            // build's frozen fallback. Read here rather than passed in, so every
            // route that builds a provider gets the same answer.
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
            cache_key: crate::kimi::is_kimi(preset.name).then(crate::kimi::cache_key),
        }
    }

    /// The HTTP agent, with the two numbers that decide when a provider that has said nothing is a
    /// failure rather than a hang: the first byte's deadline, and the stream's own budget.
    fn agent(first_byte: Duration, answer: Duration) -> ureq::Agent {
        let config = ureq::Agent::config_builder()
            // A 4xx/5xx is an answer to read, not an error to unwrap.
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(20)))
            // The response headers, on their own clock: a model API sends them with its first
            // token, so this is the silence a person waits through, and it is the short deadline.
            .timeout_recv_response(Some(first_byte))
            // No global timeout: a long answer streams for minutes and is
            // progress. `timeout_recv_body` is the stream's whole budget.
            .timeout_global(None)
            .timeout_recv_body(Some(answer))
            .build();
        ureq::Agent::new_with_config(config)
    }

    /// How long this provider may take to answer at all, over the default of
    /// [`DEFAULT_ANSWER_TIMEOUT`]. A caller that must react to a model API timeout sets its own
    /// patience here rather than waiting ten minutes for a deadline nobody chose.
    pub fn with_response_timeout(mut self, answer: Duration) -> Self {
        self.answer = answer;
        self.agent = Self::agent(self.first_byte.min(answer), answer);
        self
    }

    /// How long this provider may stay silent before its first byte, over the default of
    /// [`DEFAULT_FIRST_BYTE`]. Separate from the answer budget: a provider that says nothing is a
    /// failure in a minute, and an answer still streaming at that point is not.
    pub fn with_first_byte(mut self, first_byte: Duration) -> Self {
        self.first_byte = first_byte;
        self.agent = Self::agent(first_byte.min(self.answer), self.answer);
        self
    }

    /// The first-byte and answer deadlines in force.
    pub fn deadlines(&self) -> (Duration, Duration) {
        (self.first_byte.min(self.answer), self.answer)
    }

    /// The catalogue this provider reads, instead of the box's, so a test's answer is about
    /// this code and not about which files the machine running it happens to have.
    pub fn with_catalogue(
        mut self,
        catalogue: std::sync::Arc<crate::catalogue::Catalogue>,
    ) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// Install the way this provider tells a person something - see [`Notes`].
    pub fn with_notes(mut self, notes: Notes) -> Self {
        self.notes = Some(notes);
        self
    }

    /// What this model gets pictures as: a provider that already refused one (a measurement, and
    /// it outranks a table), then the catalogue's `modalities.input`, and otherwise the pictures are
    /// SENT, because withholding them turns every chart into a sentence about a chart.
    fn eyes(&self) -> crate::messages::Eyes<'_> {
        if crate::messages::known_text_only(self.preset.name, &self.model) {
            return crate::messages::Eyes::TextOnly(&self.model);
        }
        match self
            .catalogue
            .takes_images(self.preset.catalogue_id, &self.model)
        {
            Some(true) => crate::messages::Eyes::Sees,
            Some(false) => crate::messages::Eyes::TextOnly(&self.model),
            None => crate::messages::Eyes::Unknown(&self.model),
        }
    }

    /// The request body, for a test to look at.
    pub fn body(&self, req: &TurnRequest<'_>) -> Result<Value, BackendError> {
        self.body_for(req, self.eyes(), crate::messages::Withheld::nothing())
    }

    /// The same body, with what the model gets pictures as given rather than decided: the retry
    /// after a refusal is the one caller, and it must not re-ask a question it has just answered.
    /// `withheld` is the other half, the pictures a retry tries without and does not record.
    fn body_for(
        &self,
        req: &TurnRequest<'_>,
        eyes: crate::messages::Eyes<'_>,
        withheld: crate::messages::Withheld<'_>,
    ) -> Result<Value, BackendError> {
        // `tools` is built first because it is part of the request, NOT because it decides whether
        // the reasoning echo is required: `carries_tools` is our switch for how much to send, not a
        // rule being obeyed. A no-tools request is refused the echo for another reason entirely.
        let tools = crate::messages::tools(req.tools_json).map_err(BackendError::Malformed)?;
        let carries_tools = !tools.is_empty();
        let mut body = json!({
            "model": self.model,
            "messages": crate::messages::convert_with_tail(
                req.system,
                req.items,
                req.tail,
                carries_tools,
                eyes,
                withheld,
            ),
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if carries_tools {
            body["tools"] = Value::Array(tools);
        }
        // Kimi's implicit cache is keyed by this, and it is a hint: a miss costs a re-prefill.
        if let Some(key) = &self.cache_key {
            body["prompt_cache_key"] = json!(key);
        }
        let thinking = req.thinking.unwrap_or(self.thinking);
        // Kimi's K3.1 Pro answers `disabled` with a 400, so there the field is left out and the
        // model's own default stands (see `crate::kimi::may_switch_thinking_off`).
        if let Some(field) = self.preset.thinking_field {
            if thinking {
                body[field] = json!({"type": "enabled"});
            } else if req.thinking == Some(false)
                && crate::kimi::may_switch_thinking_off(self.preset.name, &self.model)
            {
                body[field] = json!({"type": "disabled"});
            }
        }
        if let Some(obj) = self.sampling.as_object() {
            for (k, v) in obj {
                body[k] = v.clone();
            }
        }
        // The request's cap is a ceiling the operator's sampling cannot lift: a cheap turn's
        // whole point is that it cannot run long.
        if let Some(n) = req.max_output_tokens {
            let set = body["max_tokens"].as_u64().unwrap_or(u64::MAX);
            body["max_tokens"] = json!(set.min(u64::from(n)));
        }
        // Last, because it repairs the messages the conversion above produced: Kimi refuses an
        // assistant tool-call message with no `reasoning_content` field at all.
        if crate::kimi::is_kimi(self.preset.name) {
            crate::kimi::fill_absent_thoughts(&mut body);
        }
        Ok(body)
    }
}

/// The SHAPE of a request that was refused: roles in order, which fields each message carries, and
/// how long they are. Never the content, because a log that quoted the conversation would be a
/// second copy of the transcript and every diagnosis this is for is answerable from the shape.
fn shape(body: &Value, bytes: usize) -> String {
    let mut out = String::new();
    let n_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    out.push_str(&format!(
        "    {bytes} bytes, model={} tools={n_tools} stream={} top-level-keys=[{}]",
        body.get("model").and_then(Value::as_str).unwrap_or("?"),
        body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        body.as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
    ));
    let none: Vec<Value> = Vec::new();
    let msgs = body
        .get("messages")
        .and_then(Value::as_array)
        .unwrap_or(&none);
    out.push_str(&format!("\n    {} message(s):", msgs.len()));
    for (i, m) in msgs.iter().enumerate() {
        out.push_str(&format!("\n      [{i}] {}", one_message(m)));
    }
    out
}

/// The io failure under a transport error, when there is one: a deadline must stay a deadline for
/// a caller that branches on it, and a sentence is what it must not have to read.
fn io_kind(err: &ureq::Error) -> Option<std::io::ErrorKind> {
    match err {
        ureq::Error::Timeout(_) => Some(std::io::ErrorKind::TimedOut),
        ureq::Error::Io(e) => Some(e.kind()),
        _ => None,
    }
}

/// Whether an interrupted send is asked again. One retry covers the daemon's own Ctrl-C, and
/// a loop that never gives up re-posts the whole history once per interruption: billed, and none
/// of those streams is ever read, because the first answer ends the loop.
fn ask_again(err: &ureq::Error, retried: &mut u32) -> bool {
    if let ureq::Error::Io(e) = err
        && e.kind() == std::io::ErrorKind::Interrupted
        && *retried == 0
    {
        *retried = 1;
        return true;
    }
    false
}

/// One message of a [`shape`] dump. `absent` and `null` are different answers.
fn one_message(m: &Value) -> String {
    let mut bits = vec![format!(
        "{:<9}",
        m.get("role").and_then(Value::as_str).unwrap_or("?")
    )];
    for key in ["content", "reasoning_content"] {
        bits.push(match m.get(key) {
            None => format!("{key}=absent"),
            Some(Value::Null) => format!("{key}=null"),
            Some(Value::String(s)) => format!("{key}={}ch", s.chars().count()),
            // An array is the other shape `content` takes (multi-part); its length is
            // the fact, and its parts are content.
            Some(Value::Array(a)) => format!("{key}=array({})", a.len()),
            Some(_) => format!("{key}=?"),
        });
    }
    match m.get("tool_calls").and_then(Value::as_array) {
        None => bits.push("tool_calls=absent".into()),
        Some(calls) => {
            let ids: Vec<String> = calls
                .iter()
                .map(|c| {
                    c.get("id")
                        .and_then(Value::as_str)
                        .map(|s| s.chars().take(8).collect::<String>())
                        .unwrap_or_else(|| "no-id".into())
                })
                .collect();
            bits.push(format!("tool_calls={}[{}]", calls.len(), ids.join(" ")));
        }
    }
    if let Some(id) = m.get("tool_call_id").and_then(Value::as_str) {
        bits.push(format!(
            "tool_call_id={}",
            id.chars().take(8).collect::<String>()
        ));
    }
    bits.join(" ")
}

impl MessagesBackend for OpenAiProvider {
    fn caps(&self) -> BackendCaps {
        BackendCaps::METERED_API
    }

    fn name(&self) -> &str {
        self.preset.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    /// The host the request goes to, derived from `self.url` itself, which is the very string
    /// the post uses, so this cannot name a host the request did not reach. No URL crate: every
    /// producer of this string writes `scheme://host[:port]/path`.
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
        let first = self.round(req, on_delta, eyes, crate::messages::Withheld::nothing());
        // A refusal is read off the wire, never assumed: the status is 400 and the request ACTUALLY
        // carried a picture. Two different refusals answer that shape and they are two different
        // facts. The compaction takes these paths too, which is why they exist.
        let Err(BackendError::Refused {
            status: 400,
            body: why,
            ..
        }) = &first
        else {
            return first;
        };
        let carried = crate::messages::carries_pictures(req.items.iter().chain(req.tail.iter()));
        if !eyes.takes_images() || !carried {
            return first;
        }
        // The first refusal: the MODEL takes text only. The model is marked for the rest of this
        // daemon, the operator is told once, and the same request goes again with placeholders.
        // One retry, never a ladder: a second refusal is the answer.
        if crate::messages::text_only_refusal(why) {
            if crate::messages::mark_text_only(self.preset.name, &self.model) {
                let said = format!(
                    "{} refused an image; images are now sent to it as placeholders (the provider \
                     said: {why}). The request went again without them, and this daemon will not \
                     send this model another picture.",
                    self.model
                );
                // The daemon's log first: a session may have no head attached, and this is the
                // line that says why every later request carries placeholders.
                eprintln!("  {said}");
                if let Some(notes) = &self.notes {
                    notes(said);
                }
            }
            return self.round(
                req,
                on_delta,
                crate::messages::Eyes::TextOnly(&self.model),
                crate::messages::Withheld::nothing(),
            );
        }
        // The second refusal is a different fact: the PICTURE is what it would not take. Answering
        // it with a text-only mark would withhold every picture this model was sent, so the marks go
        // against the pictures and nothing is recorded until the retry answers.
        if !crate::messages::picture_refusal(why) {
            return first;
        }
        let pictures: Vec<String> =
            crate::messages::pictures(req.items.iter().chain(req.tail.iter()))
                .into_iter()
                .map(String::from)
                .collect();
        if pictures.is_empty() {
            return first;
        }
        let trying: Vec<(String, String)> = pictures
            .iter()
            .map(|d| (crate::messages::picture_key(d), why.clone()))
            .collect();
        let second = self.round(
            req,
            on_delta,
            eyes,
            crate::messages::Withheld::trying(&trying),
        );
        let said = match &second {
            Ok(_) => {
                // Learned: every picture the refused request carried is recorded against its own
                // bytes, so no later turn earns this 400 again. The unit is the request, because a
                // good picture beside a bad one is withheld with it, which is the smaller loss.
                let mut fresh = 0;
                for d in &pictures {
                    if crate::messages::mark_bad_picture(d, why) {
                        fresh += 1;
                    }
                }
                format!(
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
                )
            }
            Err(BackendError::Refused { body, .. }) => format!(
                "{} refused a picture (the provider said: {why}), and refused the request again \
                 with every picture in it withheld (the provider said: {body}). Nothing has been \
                 recorded against the pictures: the second refusal may be about something else, \
                 and the next turn will try them again.",
                self.model
            ),
            Err(e) => format!(
                "{} refused a picture (the provider said: {why}), and the retry without pictures \
                 failed before the provider answered: {e}",
                self.model
            ),
        };
        // One notice, on both paths, and it says what actually happened rather than what was
        // attempted. The daemon's log first: a session may have no head attached.
        eprintln!("  {said}");
        if let Some(notes) = &self.notes {
            notes(said);
        }
        second
    }
}

impl OpenAiProvider {
    /// One attempt at the wire: build the body for `eyes`, post it, stream the answer. Split out
    /// because the refusal path takes the round again, and a non-2xx status returns before a byte
    /// of the body is read as a stream, which is what makes the retry safe.
    fn round(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
        eyes: crate::messages::Eyes<'_>,
        withheld: crate::messages::Withheld<'_>,
    ) -> Result<Completion, BackendError> {
        let body = self.body_for(req, eyes, withheld)?;
        // Serialised once and kept, so a refusal can say how large the request was
        // without paying for a second pass over a body that may be megabytes.
        let payload = body.to_string();
        let started = Instant::now();
        // A signal is not the provider failing: the answer bound makes this wait a timed read,
        // and a timed read ends with `EINTR` when the daemon's own Ctrl-C arrives. Nothing of the
        // answer has been read, so ask again rather than report a signal as an unreachable model.
        let mut retried = 0u32;
        let resp = loop {
            let mut posted = self
                .agent
                .post(&self.url)
                .header("Authorization", &format!("Bearer {}", self.creds.key))
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream");
            // An endpoint that admits clients by name must be told ours, and never another
            // agent's: Moonshot documents a rewritten User-Agent as a violation.
            if crate::kimi::is_kimi(self.preset.name) {
                posted = posted.header("User-Agent", crate::kimi::USER_AGENT);
            }
            match posted.send(payload.as_bytes()) {
                Ok(resp) => break resp,
                Err(e) if ask_again(&e, &mut retried) => {}
                Err(e) => {
                    return Err(BackendError::unreachable(e.to_string(), io_kind(&e)));
                }
            }
        };
        let status = resp.status().as_u16();
        // The provider's own `Retry-After`, read before the body consumes the response: seconds, and
        // the only form of *when may I come back* a caller may sleep on.
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if !(200..300).contains(&status) {
            let text = resp
                .into_body()
                .read_to_string()
                .unwrap_or_else(|e| format!("(unreadable body: {e})"));
            let parsed: Option<Value> = serde_json::from_str(&text).ok();
            let message = parsed
                .as_ref()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message").or(Some(e)))
                        .map(|m| {
                            m.as_str()
                                .map(String::from)
                                .unwrap_or_else(|| m.to_string())
                        })
                })
                .unwrap_or_else(|| text.clone());
            // The provider's own CODE, kept beside its message: taking only the message throws away
            // the one machine-readable word in the refusal, so a 429 whose code names an unpaid
            // account reaches the retry policy as prose that matches nothing in its money list.
            let message = match parsed.as_ref().and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("code"))
                    .and_then(|c| c.as_str())
            }) {
                Some(code) if !message.contains(code) => format!("{message} ({code})"),
                _ => message,
            };
            // What a 403 from the coding plan most often is, said where the operator reads the
            // refusal: the body may name nothing at all.
            let message = match crate::kimi::explain_a_403(self.preset.name, status) {
                Some(note) => format!("{message} — {note}"),
                None => message,
            };
            // What was SENT, when what was sent was refused: the shape of it and never the
            // content, so the next unexplained refusal is read off this record rather than guessed
            // at from documentation.
            eprintln!("  refused request shape:\n{}", shape(&body, payload.len()));
            return Err(BackendError::Refused {
                status,
                body: message,
                retry_after,
            });
        }
        let reader = BufReader::new(resp.into_body().into_reader());
        let mut acc = Accumulator::default();
        let mut aborted = false;
        // Whether the provider said the stream was over. A socket that closes cleanly after half an
        // answer looks exactly like a finished one, so the absence of this has to be a failure.
        let mut ended = false;
        for line in reader.lines() {
            let line = line.map_err(|e| {
                BackendError::unreachable(format!("mid-stream: {e}"), Some(e.kind()))
            })?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                ended = true;
                break;
            }
            let chunk: Value = serde_json::from_str(data)
                .map_err(|e| BackendError::Malformed(format!("{e} in chunk {data}")))?;
            // The headers carried a 200 before this byte, so an `{"error":…}` chunk is the only
            // place a mid-stream refusal can arrive; dropped, it left a half answer standing.
            if let Some(refusal) = in_band_refusal(&chunk, status, retry_after) {
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
        if !ended && acc.finish.is_none() {
            // A half answer is not an answer: recorded as `Stop` with zero usage it was billed as
            // nothing and shown as if the model had finished the thought.
            return Err(BackendError::unreachable(
                format!(
                    "the stream closed without `[DONE]` or a finish_reason after {} character(s) of \
                     answer and {} tool call(s), so what arrived is a fragment",
                    acc.text.chars().count() + acc.reasoning.chars().count(),
                    acc.calls.len()
                ),
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
}

/// The refusal a provider sends INSIDE its stream, which has no status of its own: the response's
/// is `200` because the headers went out first. `None` for a chunk that is not an error.
fn in_band_refusal(chunk: &Value, status: u16, retry_after: Option<u64>) -> Option<BackendError> {
    let err = chunk.get("error")?;
    let message = err
        .as_str()
        .map(String::from)
        .or_else(|| {
            err.get("message")
                .map(|m| m.as_str().map_or_else(|| m.to_string(), String::from))
        })
        .unwrap_or_else(|| err.to_string());
    // The provider's own code, for the reason the non-2xx arm keeps it: it is the one
    // machine-readable word in a refusal, and a quota is read off it.
    let message = match err.get("code").and_then(Value::as_str) {
        Some(code) if !message.contains(code) => format!("{message} ({code})"),
        _ => message,
    };
    // The status the chunk names when it names one, and the response's otherwise. Both are the
    // provider's own numbers; neither is invented here.
    let code = err
        .get("code")
        .and_then(Value::as_u64)
        .or_else(|| err.get("status").and_then(Value::as_u64))
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| (400..600).contains(n))
        .unwrap_or(status);
    Some(BackendError::Refused {
        status: code,
        body: message,
        retry_after,
    })
}

/// What the deltas build up, and the last chunk's usage.
#[derive(Default)]
struct Accumulator {
    text: String,
    reasoning: String,
    calls: Vec<(String, String, String)>, // (id, name, arguments) by index
    finish: Option<Finish>,
    usage: Option<Value>,
    /// The model the provider says it used, read off the stream. Not the model we asked for, and
    /// the difference is why this field exists: several names are accepted and some are aliases,
    /// so the request name is a wish and this is the fact. Cost computed from the wish reports the
    /// wrong rates for the turn that really ran.
    ///
    /// ```text
    /// asked deepseek-chat    -> every chunk says  "model": "deepseek-flash"
    /// asked deepseek-flash   -> every chunk says  "model": "deepseek-flash"
    /// ```
    ///
    /// `None` when the provider sends no `model` field at all, and then the requested name is the
    /// only thing there is to price by.
    model: Option<String>,
}

impl Accumulator {
    /// One SSE chunk → the deltas it carries, in order.
    fn absorb(&mut self, chunk: &Value) -> Vec<Delta> {
        let mut out = Vec::new();
        // Before the `choices` check below, deliberately: the served model rides on every chunk,
        // including the usage-only one that carries no choices, so reading it after that `return`
        // would miss exactly the chunk that matters most.
        if let Some(m) = chunk.get("model").and_then(|v| v.as_str())
            && !m.is_empty()
        {
            self.model = Some(m.to_string());
        }
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(u.clone());
        }
        let Some(choices) = chunk.get("choices").and_then(|c| c.as_array()) else {
            return out;
        };
        for choice in choices {
            // Kimi may put the whole usage object inside a CHOICE of its last chunk rather than at
            // the top level, and a reader that looks only above `choices` bills that turn as free.
            if let Some(u) = choice.get("usage").filter(|u| !u.is_null()) {
                self.usage = Some(u.clone());
            }
            if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                self.finish = Some(match fr {
                    "stop" => Finish::Stop,
                    "length" => Finish::Length,
                    "tool_calls" => Finish::ToolCalls,
                    other => Finish::Other(other.to_string()),
                });
            }
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            // One `let` chain rather than `if let` inside `if`: edition 2024 allows it, and the
            // nested shape is `clippy::collapsible_if`, which would fail the gate.
            if let Some(t) = delta.get("reasoning_content").and_then(|v| v.as_str())
                && !t.is_empty()
            {
                self.reasoning.push_str(t);
                out.push(Delta::Reasoning(t.to_string()));
            }
            if let Some(t) = delta.get("content").and_then(|v| v.as_str())
                && !t.is_empty()
            {
                self.text.push_str(t);
                out.push(Delta::Text(t.to_string()));
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for c in calls {
                    let index = c.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    while self.calls.len() <= index {
                        self.calls
                            .push((String::new(), String::new(), String::new()));
                    }
                    let id = c.get("id").and_then(|v| v.as_str()).map(String::from);
                    let name = c
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let args = c
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let slot = &mut self.calls[index];
                    if let Some(id) = &id {
                        slot.0 = id.clone();
                    }
                    if let Some(name) = &name {
                        slot.1.push_str(name);
                    }
                    slot.2.push_str(&args);
                    out.push(Delta::ToolCall {
                        index,
                        id,
                        name,
                        arguments: args,
                    });
                }
            }
        }
        out
    }

    fn finish(
        self,
        model: &str,
        creds: &Credentials,
        wall_ms: u64,
        preset: &'static crate::presets::Preset,
        catalogue: &crate::catalogue::Catalogue,
    ) -> Completion {
        let u = self.usage.as_ref();
        let get = |k: &str| {
            u.and_then(|u| u.get(k))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        let prompt_tokens = get("prompt_tokens");
        let generated_tokens = get("completion_tokens");
        // DeepSeek: prompt_cache_hit_tokens; OpenAI-shaped: prompt_tokens_details.cached_tokens.
        let cached_tokens = u
            .and_then(|u| u.get("prompt_cache_hit_tokens"))
            .and_then(|v| v.as_u64())
            .or_else(|| {
                u.and_then(|u| u.get("prompt_tokens_details"))
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0);
        // The operator's file first, then the catalogue, and the key is the SERVER's name: asking
        // for one model can be answered by another, so pricing the request bills the wrong rates.
        // Unpriced stays the answer when neither has it, since unpriced and free are different.
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
        let tool_calls = self
            .calls
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, arguments)| ToolCall {
                // A server that named the call is echoed back by that name; one that did not gets a
                // minted id, so two turns cannot both claim `call_0`.
                id: if id.is_empty() {
                    letibot_transcript::mint_call_id()
                } else {
                    id
                },
                name,
                arguments,
            })
            .collect();
        Completion {
            text: self.text,
            reasoning: self.reasoning,
            tool_calls,
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
            // Plain text: this API signs nothing, so its reasoning is all the row needs.
            thoughts: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One interruption is asked again and the second is not. A loop with no counter sends the
    /// whole prompt again for every signal that lands, so a held Ctrl-C bills N completions whose
    /// streams nobody reads. The rule is the whole of the bound, so the rule is what is asserted.
    #[test]
    fn an_interrupted_send_is_asked_again_once() {
        let interrupted = ureq::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "EINTR",
        ));
        let mut retried = 0;
        assert!(
            ask_again(&interrupted, &mut retried),
            "the daemon's own Ctrl-C is not the provider failing"
        );
        assert!(
            !ask_again(&interrupted, &mut retried),
            "a second interruption is a storm, and re-posting for it is a duplicate completion"
        );
        // Any other transport failure is the provider being unreachable on the first attempt.
        let down = ureq::Error::Io(std::io::Error::other("connection refused"));
        let mut fresh = 0;
        assert!(!ask_again(&down, &mut fresh));
    }

    /// The refused-request dump says what a diagnosis needs and leaks nothing: the distinctions a
    /// theory turns on (absent against present-and-null, character counts, role order, truncated
    /// call ids) and never the conversation.
    #[test]
    fn the_refused_request_shape_says_what_is_needed_and_leaks_nothing() {
        let body = json!({
            "model": "deepseek-flash",
            "stream": true,
            "messages": [
                {"role": "system", "content": "SECRET-SYSTEM-TEXT"},
                {"role": "user", "content": "SECRET-OPERATOR-WORDS"},
                {"role": "assistant", "content": Value::Null,
                 "reasoning_content": "SECRET-THINKING",
                 "tool_calls": [{"id": "call_abcdefghijkl", "type": "function",
                                 "function": {"name": "read", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "call_abcdefghijkl", "content": "SECRET-PAYLOAD"},
                // No reasoning_content key at all, which is NOT the same as an empty one.
                {"role": "assistant", "content": "ok"},
            ],
        });
        let dump = shape(&body, 4242);

        // The size, and the top-level facts.
        assert!(dump.contains("4242 bytes"), "{dump}");
        assert!(dump.contains("tools=0"), "{dump}");
        assert!(dump.contains("5 message(s)"), "{dump}");

        // Roles, in order.
        for (i, role) in ["system", "user", "assistant", "tool", "assistant"]
            .iter()
            .enumerate()
        {
            assert!(
                dump.contains(&format!("[{i}] {role}")),
                "message {i} should be a {role}: {dump}"
            );
        }

        // Absent, null and an exact length are three different answers, and the difference is why
        // this is not a summary. The figures are counted from the literals above rather than
        // guessed: a length assertion loose enough to pass for the wrong string is not one.
        assert!(dump.contains("content=null"), "a null content: {dump}");
        assert!(
            dump.contains("reasoning_content=absent"),
            "a missing key: {dump}"
        );
        assert!(dump.contains("content=18ch"), "the system text: {dump}");
        assert!(
            dump.contains("content=21ch"),
            "the operator's words: {dump}"
        );
        assert!(
            dump.contains("reasoning_content=15ch"),
            "the thinking: {dump}"
        );
        assert!(dump.contains("content=14ch"), "the payload: {dump}");
        assert!(dump.contains("content=2ch"), "the short one: {dump}");

        // The call, truncated, and the result that answers it.
        assert!(dump.contains("tool_calls=1[call_abc]"), "{dump}");
        assert!(dump.contains("tool_call_id=call_abc"), "{dump}");

        // And nothing else. Every secret above is absent.
        for secret in [
            "SECRET-SYSTEM-TEXT",
            "SECRET-OPERATOR-WORDS",
            "SECRET-THINKING",
            "SECRET-PAYLOAD",
        ] {
            assert!(
                !dump.contains(secret),
                "the dump quoted content ({secret}): {dump}"
            );
        }
    }

    /// A reply that names no call gets an id no other turn holds. The positional `call_{i}` named
    /// the first call of EVERY turn `call_0`, which is one id for a whole session; a name the server
    /// does give is still the one carried.
    #[test]
    fn two_replies_without_call_ids_mint_different_ones() {
        let creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        let id_of = |named: Option<&str>, creds: &Credentials| {
            let mut a = Accumulator::default();
            let call = match named {
                Some(id) => {
                    json!({"index": 0, "id": id, "function": {"name": "read", "arguments": "{}"}})
                }
                None => json!({"index": 0, "function": {"name": "read", "arguments": "{}"}}),
            };
            a.absorb(&json!({"choices": [{"delta": {"tool_calls": [call]}}]}));
            a.finish(
                "m",
                creds,
                1,
                &crate::presets::DEEPSEEK,
                &crate::catalogue::Catalogue::default(),
            )
            .tool_calls
            .into_iter()
            .next()
            .expect("one call")
            .id
        };
        let first = id_of(None, &creds);
        let second = id_of(None, &creds);
        assert_ne!(first, second, "the same id in two turns");
        assert_eq!(
            id_of(Some("call_srv_1"), &creds),
            "call_srv_1",
            "a server's own id is kept: the next request echoes it"
        );
    }

    #[test]
    fn deltas_accumulate_into_text_reasoning_calls_and_usage() {
        let mut a = Accumulator::default();
        let chunks = [
            json!({"choices":[{"delta":{"reasoning_content":"hm"}}]}),
            json!({"choices":[{"delta":{"content":"Hel"}}]}),
            json!({"choices":[{"delta":{"content":"lo"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"pa"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_cache_hit_tokens":60}}),
        ];
        let mut n = 0;
        for c in &chunks {
            n += a.absorb(c).len();
        }
        assert_eq!(n, 5);
        let mut creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        creds.prices.insert(
            "m".into(),
            crate::presets::Prices {
                input: 1.0,
                cached: 0.1,
                output: 2.0,
            },
        );
        let done = a.finish(
            "m",
            &creds,
            12,
            &crate::presets::DEEPSEEK,
            &crate::catalogue::Catalogue::default(),
        );
        assert_eq!(done.text, "Hello");
        assert_eq!(done.reasoning, "hm");
        assert_eq!(done.tool_calls.len(), 1);
        assert_eq!(done.tool_calls[0].id, "call_1");
        assert_eq!(done.tool_calls[0].arguments, "{\"path\":\"a\"}");
        assert_eq!(done.finish, Finish::ToolCalls);
        assert_eq!(done.cost.prompt_tokens, 100);
        assert_eq!(done.cost.cached_tokens, 60);
        // 40*1 + 60*0.1 + 7*2 = 60 micro-USD.
        assert_eq!(done.cost.micros_usd, Some(60));
        // Unpriced is None, not zero.
        let creds2 = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        assert_eq!(
            Accumulator::default()
                .finish(
                    "m",
                    &creds2,
                    0,
                    &crate::presets::DEEPSEEK,
                    &crate::catalogue::Catalogue::default()
                )
                .cost
                .micros_usd,
            None
        );
    }

    /// Kimi's last chunk may carry the usage inside its choice rather than beside `choices`, and a
    /// reader that looked only above that key billed the turn as nothing.
    #[test]
    fn a_usage_inside_a_choice_is_read_rather_than_dropped() {
        let mut a = Accumulator::default();
        a.absorb(&json!({"choices":[{"delta":{"content":"hi"}}]}));
        a.absorb(&json!({"choices":[{"delta":{},"finish_reason":"stop",
                                      "usage":{"prompt_tokens":30,"completion_tokens":4,
                                                "prompt_tokens_details":{"cached_tokens":10}}}]}));
        let creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        let done = a.finish(
            "k3",
            &creds,
            5,
            &crate::presets::KIMI_CODE,
            &crate::catalogue::Catalogue::default(),
        );
        assert_eq!(done.cost.prompt_tokens, 30);
        assert_eq!(done.cost.cached_tokens, 10);
        assert_eq!(done.cost.generated_tokens, 4);
        assert_eq!(done.text, "hi");
    }

    /// The authority is the host the request actually goes to, including when the operator's key
    /// file moved it: `creds.url` wins over the preset, so a version that read the preset would
    /// name DeepSeek while the request went to a proxy.
    #[test]
    fn the_authority_is_the_host_the_request_goes_to() {
        let plain = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        let p = OpenAiProvider::new(&crate::presets::DEEPSEEK, Some("m"), plain);
        assert_eq!(p.authority(), "api.deepseek.com");

        // The key file's own URL wins, port and all, and it is what is reported.
        let moved = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: Some("http://10.0.0.7:8443/v1/chat/completions".into()),
        };
        let p = OpenAiProvider::new(&crate::presets::DEEPSEEK, Some("m"), moved);
        assert_eq!(p.authority(), "10.0.0.7:8443");
    }

    /// The provider's own `model` field is what gets billed, not the name we asked for: a request
    /// for one model can be answered by a stream naming another, and nothing fails because an alias
    /// is supposed to resolve.
    #[test]
    fn the_served_model_is_what_gets_priced_not_the_requested_one() {
        let mut creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        // The name we ASK for - a retired alias, priced by the operator's old table.
        creds.prices.insert(
            "deepseek-chat".into(),
            crate::presets::Prices {
                input: 10.0,
                cached: 1.0,
                output: 20.0,
            },
        );
        // The name the provider ANSWERS with.
        creds.prices.insert(
            "deepseek-flash".into(),
            crate::presets::Prices {
                input: 1.0,
                cached: 0.1,
                output: 2.0,
            },
        );

        let mut a = Accumulator::default();
        // The shape of the real stream: the served name rides on every chunk.
        a.absorb(&json!({"model": "deepseek-flash", "choices": [{"delta": {"content": "hi"}}]}));
        a.absorb(&json!({"model": "deepseek-flash", "choices": [],
                         "usage": {"prompt_tokens": 1000, "completion_tokens": 100,
                                   "prompt_cache_hit_tokens": 600}}));
        let done = a.finish(
            "deepseek-chat",
            &creds,
            12,
            &crate::presets::DEEPSEEK,
            &crate::catalogue::Catalogue::default(),
        );
        assert_eq!(
            done.cost.micros_usd,
            Some(660),
            "the turn was priced from the model we asked for, not the one that served it"
        );

        // And the control: with no `model` field at all, the request name is all
        // there is to price by - so this must not have become "always unpriced".
        let mut b = Accumulator::default();
        b.absorb(&json!({"choices": [],
                         "usage": {"prompt_tokens": 1000, "completion_tokens": 100,
                                   "prompt_cache_hit_tokens": 600}}));
        let done = b.finish(
            "deepseek-chat",
            &creds,
            12,
            &crate::presets::DEEPSEEK,
            &crate::catalogue::Catalogue::default(),
        );
        assert_eq!(
            done.cost.micros_usd,
            Some(6600),
            "a silent provider still prices"
        );
    }

    /// The request the API receives carries the reasoning back, because `body` is where the
    /// tools/reasoning coupling is decided: the end-to-end half `convert`'s own tests cannot see.
    #[test]
    fn a_request_with_tools_passes_the_reasoning_back_and_one_without_does_not() {
        use letibot_transcript::{ReasoningField, SystemOrigin, TranscriptItem, UserPart};
        let items = vec![
            TranscriptItem::System {
                text: "be terse".into(),
                origin: SystemOrigin::Bootstrap,
            },
            TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "what is the date?".into(),
                }],
            },
            TranscriptItem::Reasoning {
                text: "I should call get_date.".into(),
                field: ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c1".into(),
                    name: "get_date".into(),
                    arguments: "{}".into(),
                }],
                truncated: false,
            },
        ];
        let creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        let p = OpenAiProvider::new(&crate::presets::DEEPSEEK, Some("deepseek-flash"), creds);

        let tools = vec![
            r#"{"type":"function","function":{"name":"get_date","description":"today","parameters":{"type":"object","properties":{}}}}"#
                .to_string(),
        ];
        let with = p
            .body(&TurnRequest {
                system: "be terse",
                tools_json: &tools,
                items: &items,
                tail: &[],
                max_output_tokens: None,
                thinking: None,
            })
            .expect("the body builds");
        assert!(
            with["tools"].is_array(),
            "the request must carry tools: {with:#?}"
        );
        let a = with["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .unwrap();
        assert_eq!(
            a["reasoning_content"], "I should call get_date.",
            "a request carrying tools MUST pass the reasoning back or DeepSeek \
             answers 400 — this is the defect that wedged a head at 94% context"
        );

        let without = p
            .body(&TurnRequest {
                system: "be terse",
                tools_json: &[],
                items: &items,
                tail: &[],
                max_output_tokens: None,
                thinking: None,
            })
            .expect("the body builds");
        assert!(without.get("tools").is_none(), "{without:#?}");
        let a2 = without["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .unwrap();
        assert!(
            a2.get("reasoning_content").is_none(),
            "without tools the field is not sent: {a2:#?}"
        );
    }

    /// A cheap request turns thinking off where the preset can say so, and its cap is a ceiling
    /// the operator's sampling cannot lift. `None` keeps the provider's own setting.
    #[test]
    fn a_request_can_turn_thinking_off_and_cap_the_answer() {
        let creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        let mut p = OpenAiProvider::new(&crate::presets::GLM, Some("glm-5.3"), creds);
        p.thinking = true;
        p.sampling = json!({"max_tokens": 8000});
        let req = |thinking, max_output_tokens| TurnRequest {
            system: "",
            tools_json: &[],
            items: &[],
            tail: &[],
            max_output_tokens,
            thinking,
        };
        let usual = p.body(&req(None, None)).expect("a body");
        assert_eq!(usual["thinking"], json!({"type": "enabled"}));
        assert_eq!(usual["max_tokens"], 8000);
        let cheap = p.body(&req(Some(false), Some(256))).expect("a body");
        assert_eq!(cheap["thinking"], json!({"type": "disabled"}));
        assert_eq!(cheap["max_tokens"], 256, "the cap wins over sampling");
        p.thinking = false;
        let plain = p.body(&req(None, None)).expect("a body");
        assert!(plain.get("thinking").is_none(), "{plain}");
    }

    /// K3.1 Pro answers `thinking: {"type": "disabled"}` with a 400, so a cheap turn on it asks
    /// for silence by leaving the field out: absent is the server's default, and a refusal would
    /// cost the turn. Its cache key and its empty thought ride the same body.
    #[test]
    fn a_cheap_turn_on_a_model_that_refuses_disabled_leaves_the_switch_out() {
        use letibot_transcript::{ToolCall, TranscriptItem, UserPart};
        let creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        let items = vec![
            TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text { text: "hi".into() }],
            },
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: "{}".into(),
                }],
                truncated: false,
            },
        ];
        let cheap = |model: &str| {
            let p = OpenAiProvider::new(&crate::presets::KIMI_CODE, Some(model), creds.clone());
            p.body(&TurnRequest {
                system: "",
                tools_json: &[],
                items: &items,
                tail: &[],
                max_output_tokens: Some(128),
                thinking: Some(false),
            })
            .expect("a body")
        };
        let pro = cheap("k3.1-pro");
        assert!(
            pro.get("thinking").is_none(),
            "K3.1 Pro refuses `disabled`: {pro}"
        );
        assert_eq!(pro["messages"][1]["reasoning_content"], "");
        assert!(pro["prompt_cache_key"].is_string(), "{pro}");

        let k3 = cheap("k3");
        assert_eq!(k3["thinking"], json!({"type": "disabled"}));

        // And another door is untouched: no cache key, no empty thought, the plain GLM body.
        let glm = OpenAiProvider::new(&crate::presets::GLM, Some("glm-5.3"), creds.clone());
        let body = glm
            .body(&TurnRequest {
                system: "",
                tools_json: &[],
                items: &items,
                tail: &[],
                max_output_tokens: None,
                thinking: Some(false),
            })
            .expect("a body");
        assert!(body.get("prompt_cache_key").is_none(), "{body}");
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        assert!(
            body["messages"][1].get("reasoning_content").is_none(),
            "{body}"
        );
    }
}
