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
                .unwrap_or_else(|| {
                    preset.default_model(&crate::catalogue::Catalogue::load())
                }),
            url,
            creds,
            agent: ureq::Agent::new_with_config(config),
            thinking: false,
            sampling: json!({}),
        }
    }

    /// The request body, for a test to look at.
    pub fn body(&self, req: &TurnRequest<'_>) -> Result<Value, BackendError> {
        let mut body = json!({
            "model": self.model,
            "messages": crate::messages::convert(req.system, req.items),
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        let tools = crate::messages::tools(req.tools_json).map_err(BackendError::Malformed)?;
        if !tools.is_empty() {
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

    fn complete(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
    ) -> Result<Completion, BackendError> {
        let body = self.body(req)?;
        let started = Instant::now();
        let resp = self
            .agent
            .post(&self.url)
            .header("Authorization", &format!("Bearer {}", self.creds.key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .send(body.to_string().as_bytes())
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
}

impl Accumulator {
    /// One SSE chunk → the deltas it carries, in order.
    fn absorb(&mut self, chunk: &Value) -> Vec<Delta> {
        let mut out = Vec::new();
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
        // **The operator's file first, then the catalogue.**
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
        let micros_usd = creds
            .prices
            .get(model)
            .copied()
            .or_else(|| {
                crate::catalogue::Catalogue::load()
                    .model(preset.catalogue_id, model)
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
}
