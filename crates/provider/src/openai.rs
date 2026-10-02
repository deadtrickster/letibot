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

/// One provider, one model, one key.
pub struct OpenAiProvider {
    preset: &'static Preset,
    model: String,
    url: String,
    creds: Credentials,
    agent: ureq::Agent,
    /// Ask the provider to think out loud, where it has a switch for it.
    pub thinking: bool,
    /// `temperature` and friends, passed through verbatim.
    pub sampling: Value,
}

impl OpenAiProvider {
    pub fn new(preset: &'static Preset, model: Option<&str>, creds: Credentials) -> Self {
        let url = creds.url.clone().unwrap_or_else(|| preset.url.to_string());
        let config = ureq::Agent::config_builder()
            // A 4xx/5xx is an answer to read, not an error to unwrap.
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(20)))
            // No global timeout: a long answer streams for minutes and is
            // progress. The per-read timeout is the liveness check.
            .timeout_global(None)
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .build();
        OpenAiProvider {
            preset,
            // The catalogue's pick when the operator named no model, else this
            // build's frozen fallback. Read here rather than passed in, so every
            // route that builds a provider gets the same answer.
            model: model
                .map(str::to_string)
                .unwrap_or_else(|| preset.default_model(&crate::catalogue::Catalogue::load())),
            url,
            creds,
            agent: ureq::Agent::new_with_config(config),
            thinking: false,
            sampling: json!({}),
        }
    }

    /// The request body, for a test to look at.
    pub fn body(&self, req: &TurnRequest<'_>) -> Result<Value, BackendError> {
        // **`tools` is built first because it is part of the request — NOT because it
        // decides whether the reasoning echo is required.** This comment asserted the
        // opposite for a day, on the strength of the vendor's guide, and the guide is
        // not the API: five permutations measured against the live API 2026-10-02 all
        // answered 200, including `tools` with no `reasoning_content` and no tools with
        // none either. `crate::messages`' module header carries the table.
        //
        // So `carries_tools` is **our switch for how much to send**, not a rule being
        // obeyed. It is still worth having — it keeps a large reasoning history off a
        // request that does not need it — and it is NOT what causes the
        // `reasoning_content … must be passed back` refusal: the echo is off for a
        // no-tools request, and a compaction carrying no tools was refused with exactly
        // that message. That trigger is unexplained, and a refusal now logs this
        // request's shape so the next one can be read rather than guessed at.
        let tools = crate::messages::tools(req.tools_json).map_err(BackendError::Malformed)?;
        let carries_tools = !tools.is_empty();
        let mut body = json!({
            "model": self.model,
            "messages": crate::messages::convert(req.system, req.items, carries_tools),
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if carries_tools {
            body["tools"] = Value::Array(tools);
        }
        if let Some(n) = req.max_output_tokens {
            body["max_tokens"] = json!(n);
        }
        if self.thinking
            && let Some(field) = self.preset.thinking_field
        {
            body[field] = json!({"type": "enabled"});
        }
        if let Some(obj) = self.sampling.as_object() {
            for (k, v) in obj {
                body[k] = v.clone();
            }
        }
        Ok(body)
    }
}

/// **The SHAPE of a request that was refused: roles in order, which fields each
/// message carries, and how long they are. Never the content.**
///
/// Written for a refusal nobody can explain from documentation — two readings of
/// `The reasoning_content in the thinking mode must be passed back to the API` have
/// been disproven by measurement, and the module header of [`crate::messages`] carries
/// both. What a third attempt needs is the actual request, and this is that request
/// with the conversation left out.
///
/// The distinctions below are the ones a theory can turn on and a summary would lose:
///
///   * **`size`, in bytes**, because the refusal this was written for has only ever
///     been seen on requests whose conversation was far past the model's window, and
///     the largest request this harness ever sends is a compaction of a session it
///     has already lost. A shape that cannot say how big it is cannot show that.
///   * **`absent` versus `null`.** `"content": null` is what these APIs EMIT for an
///     assistant message that only made tool calls, and a `reasoning_content` that is
///     present-and-null is a different message from one with no such key at all. If
///     the trigger is a field being the wrong KIND of empty, only this distinction
///     shows it.
///   * **Lengths**, so a message that is present-and-empty is never mistaken for one
///     that is present-and-large.
///   * **Call ids, truncated to 8 chars**, so a `tool` result can be matched to the
///     `tool_calls` entry that asked for it. A result nothing asked for is a shape
///     these APIs reject, and one whose id differs by a digit is a different shape.
///   * **Role order**, which is what "how many turns are echoed back, and which"
///     actually means.
///
/// Content is deliberately not here. A refusal log that quoted the conversation would
/// be a second copy of the transcript in a text file, and every diagnosis this is for
/// is answerable from the shape.
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

    /// **The host the request is posted to, derived from `self.url` itself.**
    ///
    /// `self.url` is `creds.url` when the operator's key file overrode it, else the
    /// preset's — and it is the very string `.post(&self.url)` uses, so this cannot name
    /// a host the request did not go to. No URL crate: every producer of this string
    /// (the preset table and the key file) writes `scheme://host[:port]/path`.
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
        let body = self.body(req)?;
        // Serialised once and kept, so a refusal can say how large the request was
        // without paying for a second pass over a body that may be megabytes.
        let payload = body.to_string();
        let started = Instant::now();
        let resp = self
            .agent
            .post(&self.url)
            .header("Authorization", &format!("Bearer {}", self.creds.key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .send(payload.as_bytes())
            .map_err(|e| BackendError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp
                .into_body()
                .read_to_string()
                .unwrap_or_else(|e| format!("(unreadable body: {e})"));
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message").or(Some(e)))
                        .map(|m| {
                            m.as_str()
                                .map(String::from)
                                .unwrap_or_else(|| m.to_string())
                        })
                })
                .unwrap_or(text);
            // **What was SENT, when what was sent was refused** — the shape of it and
            // never the content. This exists because two explanations of the
            // `reasoning_content … must be passed back` refusal have already been
            // disproven by measurement (see [`crate::messages`]' module header), and a
            // third guess would be worth no more than they were. The next occurrence
            // is read off this.
            eprintln!("  refused request shape:\n{}", shape(&body, payload.len()));
            return Err(BackendError::Refused {
                status,
                body: message,
            });
        }
        let reader = BufReader::new(resp.into_body().into_reader());
        let mut acc = Accumulator::default();
        let mut aborted = false;
        for line in reader.lines() {
            let line = line.map_err(|e| BackendError::Unreachable(format!("mid-stream: {e}")))?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let chunk: Value = serde_json::from_str(data)
                .map_err(|e| BackendError::Malformed(format!("{e} in chunk {data}")))?;
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
        let wall_ms = started.elapsed().as_millis() as u64;
        Ok(acc.finish(&self.model, &self.creds, wall_ms, self.preset))
    }
}

/// What the deltas build up, and the last chunk's usage.
#[derive(Default)]
struct Accumulator {
    text: String,
    reasoning: String,
    calls: Vec<(String, String, String)>, // (id, name, arguments) by index
    finish: Option<Finish>,
    usage: Option<Value>,
    /// **The model the provider says it used**, read off the stream.
    ///
    /// Not the model we asked for, and the difference is the whole reason this field
    /// exists. MEASURED 2026-10-02, this account:
    ///
    /// ```text
    /// asked deepseek-chat    -> every chunk says  "model": "deepseek-flash"
    /// asked deepseek-flash   -> every chunk says  "model": "deepseek-flash"
    /// ```
    ///
    /// Six names are accepted and four are aliases, so the request name is a *wish*
    /// and this is the fact. Cost was computed from the wish: `providers.toml` prices
    /// `deepseek-chat` at 0.28/0.028/0.42 while the model actually serving it is
    /// `deepseek-flash` at 0.15/0.003/0.6 — so a session on the retired alias reported
    /// roughly twice the input rate and five times the cache rate of the turn it
    /// really ran. `None` when the provider sends no `model` field at all, and then the
    /// requested name is the only thing there is to price by.
    model: Option<String>,
}

impl Accumulator {
    /// One SSE chunk → the deltas it carries, in order.
    fn absorb(&mut self, chunk: &Value) -> Vec<Delta> {
        let mut out = Vec::new();
        // **Before the `choices` check below, deliberately.** The served model rides on
        // every chunk including the usage-only one that carries no choices, and a
        // version that read it after that `return` would miss exactly the chunks that
        // matter least and the field that matters most.
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
            if let Some(t) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    self.reasoning.push_str(t);
                    out.push(Delta::Reasoning(t.to_string()));
                }
            }
            if let Some(t) = delta.get("content").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    self.text.push_str(t);
                    out.push(Delta::Text(t.to_string()));
                }
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
        // **The operator's file first, then the catalogue — and the SERVER's name, not
        // ours.**
        //
        // `providers.toml` prices `deepseek-chat`, which is a model DeepSeek has
        // retired — so every turn on `deepseek-flash` reported `cost unpriced`,
        // and the operator watching a metered conversation: *"also no money
        // meter"*. A hand-maintained table goes stale exactly where a default
        // model does, and for the same reason.
        //
        // The file still wins where it has an entry, because that is the
        // operator's own number and they may be on a contract price. Absent one,
        // models.dev has the published rate, which is a better answer than
        // "unpriced" — and "unpriced" stays the answer when NEITHER has it, since
        // unpriced and free are different.
        //
        // **And the key is the served model, because the requested one is a wish.**
        // MEASURED 2026-10-02: asking for `deepseek-chat` is answered by a stream whose
        // every chunk says `"model": "deepseek-flash"`. Pricing the wish billed the
        // retired alias's rate — 0.28/0.028/0.42 — for a turn the live model served at
        // 0.15/0.003/0.6. When the provider sends no name, the requested one is all
        // there is and is used; when it sends one that neither the file nor the
        // catalogue prices, the turn is **unpriced**, which is the honest answer and
        // the one this tree already prefers to a wrong number.
        let billed = self.model.as_deref().unwrap_or(model);
        let micros_usd = creds
            .prices
            .get(billed)
            .copied()
            .or_else(|| {
                crate::catalogue::Catalogue::load()
                    .model(preset.catalogue_id, billed)
                    .and_then(|m| m.prices)
            })
            .map(|p| p.micros(prompt_tokens, cached_tokens, generated_tokens));
        let tool_calls = self
            .calls
            .into_iter()
            .enumerate()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(i, (id, name, arguments))| ToolCall {
                id: if id.is_empty() {
                    format!("call_{i}")
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The refused-request dump says what a diagnosis needs and leaks nothing.**
    ///
    /// This is the record two disproven theories are handed over to — see
    /// `crate::messages`' module header for the five live-API permutations that killed
    /// the second one. Because it exists to END a guessing game, the test is about the
    /// two ways it could mislead rather than about its formatting:
    ///
    ///   * it must carry the distinctions a theory turns on — a field that is ABSENT
    ///     against one that is present-and-null, a count of characters for each part,
    ///     the role order, and the truncated call ids that pair a result to its call;
    ///   * and it must not carry the conversation. A refusal log that quoted the model
    ///     and the operator's own words would be a second copy of the transcript in a
    ///     plain text file, and every diagnosis this is for is answerable without it.
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

        // **Absent, null, and an exact length — three different answers**, and the
        // difference is the whole reason this is not a summary. The figures are counted
        // from the literals above rather than guessed at: a length assertion that is
        // loose enough to pass for the wrong string is not an assertion.
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
        let done = a.finish("m", &creds, 12, &crate::presets::DEEPSEEK);
        assert_eq!(done.text, "Hello");
        assert_eq!(done.reasoning, "hm");
        assert_eq!(done.tool_calls.len(), 1);
        assert_eq!(done.tool_calls[0].id, "call_1");
        assert_eq!(done.tool_calls[0].arguments, "{\"path\":\"a\"}");
        assert_eq!(done.finish, Finish::ToolCalls);
        assert_eq!(done.cost.prompt_tokens, 100);
        assert_eq!(done.cost.cached_tokens, 60);
        // 40*1 + 60*0.1 + 7*2 = 60 per million → 60 micro-USD
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
                .finish("m", &creds2, 0, &crate::presets::DEEPSEEK)
                .cost
                .micros_usd,
            None
        );
    }

    /// **The authority is the host the request actually goes to, including when the
    /// operator's key file moved it.**
    ///
    /// This exists because a warning about a failed round named the daemon's LOCAL
    /// endpoint on a cloud turn; see `MessagesBackend::authority`. The override case is
    /// the one that matters: `creds.url` wins over the preset, so the reported host has
    /// to follow the URL — a version that read `preset.url` would name DeepSeek while the
    /// request went to a proxy.
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

    /// **The provider's own `model` field is what gets billed, not the name we asked
    /// for.**
    ///
    /// MEASURED 2026-10-02 against this account: `POST` with `model: deepseek-chat` is
    /// answered by a stream in which every chunk says `"model": "deepseek-flash"`.
    /// Cost was computed from the request, so a turn the live model served at
    /// 0.15/0.003/0.6 was billed at the retired alias's 0.28/0.028/0.42 — and nothing
    /// failed, because an alias is *supposed* to resolve.
    ///
    /// The two rates here are an order of magnitude apart so the assertion cannot pass
    /// by arithmetic accident: 1000 prompt of which 600 cached and 100 out is 660
    /// micro-USD at the served rate and 6600 at the requested one.
    #[test]
    fn the_served_model_is_what_gets_priced_not_the_requested_one() {
        let mut creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        // The name we ASK for — a retired alias, priced by the operator's old table.
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
        let done = a.finish("deepseek-chat", &creds, 12, &crate::presets::DEEPSEEK);
        assert_eq!(
            done.cost.micros_usd,
            Some(660),
            "the turn was priced from the model we asked for, not the one that served it"
        );

        // **And the control: with no `model` field at all, the request name is all
        // there is to price by** — so this must not have become "always unpriced".
        let mut b = Accumulator::default();
        b.absorb(&json!({"choices": [],
                         "usage": {"prompt_tokens": 1000, "completion_tokens": 100,
                                   "prompt_cache_hit_tokens": 600}}));
        let done = b.finish("deepseek-chat", &creds, 12, &crate::presets::DEEPSEEK);
        assert_eq!(
            done.cost.micros_usd,
            Some(6600),
            "a silent provider still prices"
        );
    }

    /// **The request the API actually receives carries the reasoning back, because
    /// `body` is where the `tools`/reasoning coupling is decided.**
    ///
    /// This is the assertion that was missing end to end. `convert`'s own tests say
    /// what the SHAPE is when told to echo; this says that the request carrying tools
    /// is the one that gets told — which is the half a provider-name guess would have
    /// got wrong, since both DeepSeek models disagree behind one name.
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
                max_output_tokens: None,
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
                max_output_tokens: None,
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
}
