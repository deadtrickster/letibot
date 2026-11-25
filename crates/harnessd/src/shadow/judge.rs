//! The model a shadow job asks: any configured provider, or a local chat endpoint. No tools. A
//! stateless job sends one message per call; a persistent one sends its running conversation.

use std::sync::Arc;

use letibot_turn::http::Endpoint;

use super::Job;

/// Most a verdict may spend. The schema is one short object.
pub const MAX_TOKENS: u32 = 800;

/// What a call cost, when the model said: the prompt and how much of it was served from cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
}

/// One judgement: the prompt in, the reply text out.
pub trait Judge: Send + Sync {
    /// `provider/model`, or `local`, for every row the shadow writes.
    fn label(&self) -> String;
    fn judge(&self, system: &str, user: &str) -> Result<String, String>;

    /// One turn of a running conversation. A judge with no conversation of its own is sent the
    /// newest message alone.
    fn converse(
        &self,
        system: &str,
        messages: &[Message],
    ) -> Result<(String, Option<Usage>), String> {
        let last = messages.last().map(|m| m.text.as_str()).unwrap_or("");
        self.judge(system, last).map(|r| (r, None))
    }
}

use super::conversation::{Message, Role};

/// A provider backend, seated for one-shot verdicts.
struct ProviderJudge {
    label: String,
    backend: Box<dyn letibot_backend::MessagesBackend>,
}

impl Judge for ProviderJudge {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn judge(&self, system: &str, user: &str) -> Result<String, String> {
        let one = [Message {
            role: Role::User,
            text: user.into(),
        }];
        self.converse(system, &one).map(|(r, _)| r)
    }

    fn converse(
        &self,
        system: &str,
        messages: &[Message],
    ) -> Result<(String, Option<Usage>), String> {
        let items: Vec<letibot_transcript::TranscriptItem> = messages
            .iter()
            .map(|m| match m.role {
                Role::User => letibot_transcript::TranscriptItem::User {
                    speaker: letibot_transcript::Speaker::Operator,
                    parts: vec![letibot_transcript::UserPart::Text {
                        text: m.text.clone(),
                    }],
                },
                Role::Assistant => letibot_transcript::TranscriptItem::Assistant {
                    text: m.text.clone(),
                    tool_calls: Vec::new(),
                    truncated: false,
                },
            })
            .collect();
        let req = letibot_backend::TurnRequest {
            system,
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: Some(MAX_TOKENS),
            thinking: Some(false),
            json: true,
        };
        self.backend
            .complete(&req, &mut |_| letibot_backend::StreamFlow::Continue)
            .map(|c| {
                let usage = Usage {
                    prompt_tokens: c.cost.prompt_tokens,
                    cached_tokens: c.cost.cached_tokens,
                };
                (reply_of(&c.text, &c.reasoning), Some(usage))
            })
            .map_err(|e| e.to_string())
    }
}

/// The reply a verdict is read from: the answer, or, when a model that kept thinking answered
/// nothing, the JSON object its reasoning ends with. Either way it meets the same checks.
pub fn reply_of(text: &str, reasoning: &str) -> String {
    if !text.trim().is_empty() {
        return text.to_string();
    }
    match (reasoning.rfind('{'), reasoning.rfind('}')) {
        (Some(_), Some(_)) => last_object(reasoning).unwrap_or_default().to_string(),
        _ => String::new(),
    }
}

/// The last balanced `{...}` in `s`, outside string literals.
fn last_object(s: &str) -> Option<&str> {
    let end = s.rfind('}')?;
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = end + 1;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b'}' => depth += 1,
            b'{' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[i..=end]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A llama-server style chat endpoint: the daemon's own server or a declared `[model.*]` one.
struct LocalJudge {
    label: String,
    endpoint: Endpoint,
    model: String,
}

impl Judge for LocalJudge {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn judge(&self, system: &str, user: &str) -> Result<String, String> {
        let one = [Message {
            role: Role::User,
            text: user.into(),
        }];
        self.converse(system, &one).map(|(r, _)| r)
    }

    fn converse(
        &self,
        system: &str,
        messages: &[Message],
    ) -> Result<(String, Option<Usage>), String> {
        let mut msgs = vec![serde_json::json!({ "role": "system", "content": system })];
        msgs.extend(messages.iter().map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            serde_json::json!({ "role": role, "content": m.text })
        }));
        let body = serde_json::json!({
            "model": self.model,
            "messages": msgs,
            "cache_prompt": true,
            "max_tokens": MAX_TOKENS,
            "temperature": 0.0,
            "reasoning_effort": "none",
            "chat_template_kwargs": { "enable_thinking": false },
            "response_format": { "type": "json_object" },
        })
        .to_string();
        let res = letibot_turn::http::post_json(&self.endpoint, "/v1/chat/completions", &body)
            .map_err(|e| e.to_string())?;
        let text = res.read_to_string().map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let field = |f: &str| {
            v.pointer(&format!("/choices/0/message/{f}"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string()
        };
        if v.pointer("/choices/0/message").is_none() {
            return Err("the reply carries no choices[0].message".to_string());
        }
        let n = |p: &str| v.pointer(p).and_then(|x| x.as_u64());
        let usage = n("/usage/prompt_tokens").map(|prompt_tokens| Usage {
            prompt_tokens,
            cached_tokens: n("/usage/prompt_tokens_details/cached_tokens")
                .or_else(|| n("/timings/cache_n"))
                .unwrap_or(0),
        });
        Ok((
            reply_of(&field("content"), &field("reasoning_content")),
            usage,
        ))
    }
}

/// The judge for `spelling`, as `/models` spells a model: `local`, a declared local model, or
/// `provider[/model]`. Its transport deadline is the job's, so a slow call ends on its own.
pub fn build(
    spelling: &str,
    job: Job,
    providers_file: Option<&std::path::Path>,
    local: &Endpoint,
) -> Result<Arc<dyn Judge>, String> {
    build_timed(spelling, providers_file, local, job.deadline())
}

/// [`build`] with a deadline of the caller's choosing: `--shadow-arm` measures a job whose live
/// budget is 2.5 s, and a measured call has no turn behind it to protect.
pub fn build_timed(
    spelling: &str,
    providers_file: Option<&std::path::Path>,
    local: &Endpoint,
    deadline: std::time::Duration,
) -> Result<Arc<dyn Judge>, String> {
    let m = crate::harness::subagent_model_in(providers_file, spelling, None)
        .map_err(|lines| lines.join("; "))?;
    if let Some(pc) = m.provider {
        let notes: letibot_provider::openai::Notes = Arc::new(|_| {});
        let deadlines = crate::leticode_config::Deadlines {
            slow_notice: deadline,
            first_byte: deadline,
            answer: deadline,
        };
        // A judge's refusal is the judge's answer, not the session's log: it is said where the
        // judge's verdict is read.
        let backend = crate::harness::build_provider(
            &pc,
            providers_file,
            notes,
            deadlines,
            None,
            None,
            letibot_provider::budget::Budget::NONE,
        )?;
        return Ok(Arc::new(ProviderJudge {
            label: m.label,
            backend,
        }));
    }
    let (mut endpoint, model) = match m.local {
        Some(l) => (crate::harness::parse_local_url(&l.url)?, l.model),
        None => (local.clone(), "local".to_string()),
    };
    endpoint.read_timeout = deadline;
    endpoint.connect_timeout = endpoint.connect_timeout.min(deadline);
    Ok(Arc::new(LocalJudge {
        label: m.label,
        endpoint,
        model,
    }))
}

/// Used where a job is on and its model cannot be built: every call fails with the reason, so the
/// breaker pauses the job with one visible row instead of the session failing to open.
pub struct Unbuildable {
    pub label: String,
    pub why: String,
}

impl Judge for Unbuildable {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn judge(&self, _: &str, _: &str) -> Result<String, String> {
        Err(self.why.clone())
    }
}
