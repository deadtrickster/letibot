//! The model a shadow job asks: any configured provider, or a local chat endpoint. One call, no
//! tools, no history - the shadow session is a record, not a conversation.

use std::sync::Arc;

use letibot_turn::http::Endpoint;

use super::Job;

/// Most a verdict may spend. The schema is one short object.
pub const MAX_TOKENS: u32 = 800;

/// One judgement: the prompt in, the reply text out.
pub trait Judge: Send + Sync {
    /// `provider/model`, or `local`, for every row the shadow writes.
    fn label(&self) -> String;
    fn judge(&self, system: &str, user: &str) -> Result<String, String>;
}

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
        let items = [letibot_transcript::TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![letibot_transcript::UserPart::Text { text: user.into() }],
        }];
        let req = letibot_backend::TurnRequest {
            system,
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: Some(MAX_TOKENS),
            thinking: Some(false),
        };
        self.backend
            .complete(&req, &mut |_| letibot_backend::StreamFlow::Continue)
            .map(|c| c.text)
            .map_err(|e| e.to_string())
    }
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
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "max_tokens": MAX_TOKENS,
            "temperature": 0.0,
            "reasoning_effort": "none",
            "chat_template_kwargs": { "enable_thinking": false },
        })
        .to_string();
        let res = letibot_turn::http::post_json(&self.endpoint, "/v1/chat/completions", &body)
            .map_err(|e| e.to_string())?;
        let text = res.read_to_string().map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        v.pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .map(str::to_string)
            .ok_or_else(|| "the reply carries no choices[0].message.content".to_string())
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
    let m = crate::harness::subagent_model_in(providers_file, spelling, None)
        .map_err(|lines| lines.join("; "))?;
    let deadline = job.deadline();
    if let Some(pc) = m.provider {
        let notes: letibot_provider::openai::Notes = Arc::new(|_| {});
        let deadlines = crate::leticode_config::Deadlines {
            slow_notice: deadline,
            first_byte: deadline,
            answer: deadline,
        };
        let backend = crate::harness::build_provider(&pc, providers_file, notes, deadlines, None)?;
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
