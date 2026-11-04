//! The `/completion` fallback request and its replies. `prompt` is a token array: the harness
//! tokenizes, the server does not. One constructor sets every field; there is no `n_predict`,
//! since a cap is a chosen truncation.

use serde::Serialize;
use serde_json::Value;

use letibot_tokencore::TokenId;

/// The prompt, in one of the two shapes llama.cpp takes: `Ids` is the bare array (byte-identical
/// on the wire, which is a requirement), and `WithMedia` is used only because `/completion`
/// honours `multimodal_data` only for a `prompt_string`, which the server tokenizes itself.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Prompt {
    /// Token ids. Not a string: the harness owns rendering and tokenization, and a
    /// string here would hand both back to a jinja renderer nobody controls.
    Ids(Vec<TokenId>),
    /// A prompt with a picture in it, as the text a server tokenizes itself. `prompt_string`
    /// carries the server's own media marker where each image sits; `multimodal_data` carries
    /// the matching base64 payloads without their `data:` prefix, in the same order.
    WithMedia {
        prompt_string: String,
        multimodal_data: Vec<String>,
    },
}

/// The body of a `/completion` call, exactly as §5.6 specifies it.
#[derive(Debug, Clone, Serialize)]
pub struct CompletionRequest {
    pub prompt: Prompt,
    pub cache_prompt: bool,
    pub return_progress: bool,
    pub timings_per_token: bool,
    pub return_tokens: bool,
    pub stream: bool,
    #[serde(flatten)]
    pub sampling: Value,
}

impl CompletionRequest {
    /// The one constructor: every §5.6 field is set here rather than defaulted. No `stop` field,
    /// on purpose - `/completion` takes stop strings only, so stops are enforced by id against
    /// the set resolved at startup, and a one-entry boundary is never missed as text would be.
    pub fn new(prompt: Vec<TokenId>, sampling: Value) -> Self {
        CompletionRequest {
            prompt: Prompt::Ids(prompt),
            cache_prompt: true,
            return_progress: true,
            timings_per_token: true,
            return_tokens: true,
            stream: true,
            sampling,
        }
    }

    /// The same request, for a turn that carries a picture. `marker` is the server's own, from
    /// `/props`, never a dialect's vision tokens; `media` are the base64 payloads in the order
    /// their markers appear.
    pub fn with_media(prompt_string: String, media: Vec<String>, sampling: Value) -> Self {
        CompletionRequest {
            prompt: Prompt::WithMedia {
                prompt_string,
                multimodal_data: media,
            },
            cache_prompt: true,
            return_progress: true,
            timings_per_token: true,
            return_tokens: true,
            stream: true,
            sampling,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a completion request always serialises")
    }
}

/// `result_prompt_progress` (`server-task.h:264-271`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PromptProgress {
    pub total: u64,
    pub cache: u64,
    pub processed: u64,
    pub time_ms: u64,
}

/// The `timings` object, per chunk under `timings_per_token`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Timings {
    /// Prompt tokens reused from the cache - the field the prefix invariant is measured
    /// against, and not `tokens_cached`, which is the slot's occupancy after generation and
    /// would inflate `f_keep` by the whole generation.
    pub cache_n: u64,
    pub prompt_n: u64,
    pub prompt_ms: f64,
    pub predicted_n: u64,
    pub predicted_ms: f64,
    pub draft_n: u64,
    pub draft_n_accepted: u64,
}

impl Timings {
    fn from_json(v: &Value) -> Timings {
        let g = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let u = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Timings {
            cache_n: u("cache_n"),
            prompt_n: u("prompt_n"),
            prompt_ms: g("prompt_ms"),
            predicted_n: u("predicted_n"),
            predicted_ms: g("predicted_ms"),
            draft_n: u("draft_n"),
            draft_n_accepted: u("draft_n_accepted"),
        }
    }
}

/// Why generation stopped. `Length` is a variant, never a flag on a success: nine turns that
/// had spent their whole budget thinking were once recorded as completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// The model emitted an end-of-generation token.
    Eos,
    /// A stop token or stop word fired.
    Word,
    /// The output limit was reached. Acted on, see [`crate::length`].
    Length,
    /// The turn was aborted by us - a guard, or an urgent steering message.
    Aborted,
    /// A value the server produced that this enum does not model. Kept verbatim
    /// rather than folded into a neighbour, because a `finish_reason` nobody
    /// recognises is exactly the thing that must not be silently normalised.
    Other(&'static str),
}

impl FinishReason {
    fn parse(s: &str) -> FinishReason {
        match s {
            "eos" => FinishReason::Eos,
            "word" => FinishReason::Word,
            "limit" | "length" => FinishReason::Length,
            "none" => FinishReason::Other("none"),
            _ => FinishReason::Other("unrecognised"),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FinishReason::Eos => "eos",
            FinishReason::Word => "word",
            FinishReason::Length => "length",
            FinishReason::Aborted => "aborted",
            FinishReason::Other(s) => s,
        }
    }
}

/// One decoded SSE frame, classified. The classification is the point: a naive reader sees "a
/// chunk with a `tokens` array" three times before the first real token, each carrying a
/// fabricated token id 0 - see [`crate::stream`].
#[derive(Debug, Clone, PartialEq)]
pub enum Chunk {
    /// Prompt processing progress. Carries no generated output, whatever its
    /// `tokens` field says.
    Progress {
        progress: PromptProgress,
        timings: Option<Timings>,
    },
    /// One (or more) generated tokens.
    Token {
        ids: Vec<TokenId>,
        text: String,
        /// `tokens_predicted` - the server's own monotone count of generated
        /// tokens *including this chunk*. This is the field that makes the
        /// accumulator safe, because it is the fact rather than a proxy for it.
        n_decoded: u64,
        timings: Option<Timings>,
    },
    /// The terminal frame. Its `tokens` array is empty in stream mode.
    Final(Box<FinalChunk>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinalChunk {
    /// Which slot served the turn. Not decoration: on a five-slot box a turn that lands on a
    /// different slot from the one holding its history reports a cache reuse of zero, which is
    /// a scheduling fact rather than a prefix divergence.
    pub id_slot: i64,
    pub finish_reason: FinishReason,
    pub stopping_word: String,
    /// The server's own `truncated` flag: the prompt did not fit and was cut.
    /// Different from a `length` finish and not interchangeable with it.
    pub prompt_truncated: bool,
    pub n_prompt_tokens: u64,
    pub n_decoded: u64,
    /// `slot.prompt.n_tokens()` after generation. Occupancy, not reuse.
    pub slot_tokens_after: u64,
    pub timings: Timings,
    /// Whatever ids the final frame carried. Empty in stream mode - that is the
    /// trap this type exists to make visible rather than to hide.
    pub ids: Vec<TokenId>,
    pub text: String,
}

/// Turn one SSE `data:` payload into a [`Chunk`]. Not "does it have `prompt_progress`": a
/// progress frame is built from a default-constructed token output and carries a fabricated id
/// 0. Classify on `tokens_predicted` - 0 there, strictly increasing on a real frame.
pub fn classify(payload: &str) -> Result<Chunk, String> {
    let v: Value = serde_json::from_str(payload).map_err(|e| format!("{e}: {payload}"))?;
    if let Some(err) = v.get("error") {
        return Err(format!("server error frame: {err}"));
    }
    let timings = v.get("timings").map(Timings::from_json);
    let n_decoded = v
        .get("tokens_predicted")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    if v.get("stop").and_then(Value::as_bool) == Some(true) {
        return Ok(Chunk::Final(Box::new(FinalChunk {
            id_slot: v.get("id_slot").and_then(Value::as_i64).unwrap_or(-1),
            finish_reason: FinishReason::parse(
                v.get("stop_type").and_then(Value::as_str).unwrap_or("none"),
            ),
            stopping_word: v
                .get("stopping_word")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            prompt_truncated: v.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            n_prompt_tokens: v
                .get("tokens_evaluated")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            n_decoded,
            slot_tokens_after: v.get("tokens_cached").and_then(Value::as_u64).unwrap_or(0),
            timings: timings.unwrap_or_default(),
            ids: ids_of(&v),
            text: v
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })));
    }

    if n_decoded == 0 {
        let p = v.get("prompt_progress");
        return Ok(Chunk::Progress {
            progress: PromptProgress {
                total: field(p, "total"),
                cache: field(p, "cache"),
                processed: field(p, "processed"),
                time_ms: field(p, "time_ms"),
            },
            timings,
        });
    }

    Ok(Chunk::Token {
        ids: ids_of(&v),
        text: v
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        n_decoded,
        timings,
    })
}

fn field(v: Option<&Value>, key: &str) -> u64 {
    v.and_then(|v| v.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn ids_of(v: &Value) -> Vec<TokenId> {
    v.get("tokens")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_u64)
                .map(|n| n as u32)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request shape is a contract with §5.6, so it is checked as one.
    #[test]
    fn every_field_the_plan_names_is_on_the_wire() {
        let req = CompletionRequest::new(vec![1, 2, 3], serde_json::json!({"temperature": 0}));
        let v: Value = serde_json::from_str(&req.to_json()).unwrap();
        assert_eq!(v["prompt"], serde_json::json!([1, 2, 3]));
        assert!(
            v["prompt"].is_array(),
            "prompt must be a token array, not text"
        );
        assert_eq!(v["cache_prompt"], true);
        assert_eq!(v["return_progress"], true);
        assert_eq!(v["timings_per_token"], true);
        assert_eq!(v["return_tokens"], true);
        assert_eq!(v["stream"], true);
        assert_eq!(v["temperature"], 0);
        // Stops are enforced by token id on our side; a stop *string* would be
        // matched against decoded text, which is the boundary-missing failure.
        assert!(v.get("stop").is_none(), "no stop strings on the wire");
    }

    /// §5.7: there is no output wall. A cap is a truncation you chose, and this is
    /// the line that stops one being added back as "just a safety default".
    #[test]
    fn there_is_no_n_predict_cap_anywhere_in_the_body() {
        let req = CompletionRequest::new(vec![1], serde_json::json!({}));
        let body = req.to_json();
        assert!(!body.contains("n_predict"), "{body}");
        assert!(!body.contains("max_tokens"), "{body}");
    }

    #[test]
    fn a_progress_frame_is_not_a_token_frame_however_it_spells_its_tokens_array() {
        // Verbatim from this box's llama.cpp, `return_progress: true`.
        let raw = r#"{"index":0,"content":"","tokens":[0],"stop":false,"id_slot":-1,
            "tokens_predicted":0,"tokens_evaluated":20,
            "prompt_progress":{"total":20,"cache":0,"processed":16,"time_ms":71}}"#;
        match classify(raw).unwrap() {
            Chunk::Progress { progress, .. } => {
                assert_eq!(progress.processed, 16);
                assert_eq!(progress.total, 20);
            }
            other => panic!("a fabricated token id 0 was taken for output: {other:?}"),
        }
    }

    #[test]
    fn the_final_frame_reports_reuse_from_timings_not_from_tokens_cached() {
        let raw = r#"{"index":0,"content":"","tokens":[],"stop":true,
            "tokens_predicted":17,"tokens_evaluated":20,"tokens_cached":38,
            "stop_type":"eos","stopping_word":"","truncated":false,
            "timings":{"cache_n":0,"prompt_n":20,"prompt_ms":129.4,"predicted_n":17,
                       "predicted_ms":129.1,"draft_n":15,"draft_n_accepted":15}}"#;
        let Chunk::Final(f) = classify(raw).unwrap() else {
            panic!("not final")
        };
        assert_eq!(f.timings.cache_n, 0, "reuse");
        assert_eq!(
            f.slot_tokens_after, 38,
            "occupancy, and it is not the reuse"
        );
        assert_eq!(f.n_decoded, 17);
        assert!(
            f.ids.is_empty(),
            "the trap: stream mode's final frame is empty"
        );
        assert_eq!(f.finish_reason, FinishReason::Eos);
    }

    #[test]
    fn an_unrecognised_finish_reason_is_not_folded_into_a_neighbour() {
        let raw = r#"{"stop":true,"stop_type":"something_new","tokens_predicted":1}"#;
        let Chunk::Final(f) = classify(raw).unwrap() else {
            panic!()
        };
        assert_eq!(f.finish_reason, FinishReason::Other("unrecognised"));
        assert_ne!(f.finish_reason, FinishReason::Eos);
    }
}
