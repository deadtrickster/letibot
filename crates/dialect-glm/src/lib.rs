//! GLM-5.3-Flash as data: [`glm_spec`] carries the shipped jinja, its sha, the control and
//! stop tokens, the system-update rule and guards, and a prompt is rendered by running
//! that template, not by code. We render the Jinja2 reading of a turn, which is what the
//! model was trained against:
//! ```text
//! server: …<|assistant|><think>R1</think>one<|user|>b<|assistant|><think>R1</think>two…
//! jinja2: …<|assistant|><think>R1</think>one<|user|>b<|assistant|><think></think>two…
//! ```

pub mod cli;
use std::sync::OnceLock;

use letibot_dialect::{
    ControlRole, ControlToken, ControlTokens, DialectSpec, Guard, ReasoningField, StopToken,
    SystemUpdateMode,
};

mod json;
mod parse;
mod render;
mod sha256;

pub use json::{arg_value_text, glm_tool_json, hf_tojson};
pub use parse::{GlmParser, TableDecoder};
pub use render::{
    Anomaly, GlmRenderer, check_transcript, ends_mid_turn, generation_prompt,
    generation_prompt_closing_reasoning, outcome_envelope,
};

/// The shipped jinja, verbatim: the renderer's input, not documentation.
/// Extracted with `python3 tests/fidelity/extract_template.py <gguf>`.
pub const TEMPLATE: &str = include_str!("../template/glm-5.3-flash.jinja");

/// The name this dialect is known by, in the gate and in the store.
pub const NAME: &str = "glm-5.3-flash";

/// The literal spelling of every GLM token this dialect emits.
/// Each is a single vocab entry, `CONTROL` or `USER_DEFINED`, which is why it is emitted
/// as `RenderSpan::Control` and never as text.
pub mod tokens {
    use letibot_dialect::{ControlRole, ControlToken};

    const fn t(role: ControlRole, literal: &'static str) -> ControlToken {
        ControlToken::borrowed(role, literal)
    }

    pub const GMASK: ControlToken = t(ControlRole::BeginOfText, "[gMASK]");
    pub const SOP: ControlToken = t(ControlRole::SequenceStart, "<sop>");
    pub const SYSTEM: ControlToken = t(ControlRole::TurnStartSystem, "<|system|>");
    pub const USER: ControlToken = t(ControlRole::TurnStartUser, "<|user|>");
    pub const ASSISTANT: ControlToken = t(ControlRole::TurnStartAssistant, "<|assistant|>");
    pub const OBSERVATION: ControlToken = t(ControlRole::TurnStartTool, "<|observation|>");
    pub const THINK_OPEN: ControlToken = t(ControlRole::ThinkOpen, "<think>");
    pub const THINK_CLOSE: ControlToken = t(ControlRole::ThinkClose, "</think>");
    pub const TOOL_CALL_OPEN: ControlToken = t(ControlRole::ToolCallOpen, "<tool_call>");
    pub const TOOL_CALL_CLOSE: ControlToken = t(ControlRole::ToolCallClose, "</tool_call>");
    pub const TOOL_RESPONSE_OPEN: ControlToken = t(ControlRole::ToolResultOpen, "<tool_response>");
    pub const TOOL_RESPONSE_CLOSE: ControlToken =
        t(ControlRole::ToolResultClose, "</tool_response>");
    pub const ARG_KEY_OPEN: ControlToken = t(ControlRole::ArgKeyOpen, "<arg_key>");
    pub const ARG_KEY_CLOSE: ControlToken = t(ControlRole::ArgKeyClose, "</arg_key>");
    pub const ARG_VALUE_OPEN: ControlToken = t(ControlRole::ArgValueOpen, "<arg_value>");
    pub const ARG_VALUE_CLOSE: ControlToken = t(ControlRole::ArgValueClose, "</arg_value>");
    pub const BEGIN_OF_IMAGE: ControlToken = t(ControlRole::ImageOpen, "<|begin_of_image|>");
    pub const IMAGE: ControlToken = t(ControlRole::Image, "<|image|>");
    pub const END_OF_IMAGE: ControlToken = t(ControlRole::ImageClose, "<|end_of_image|>");
    pub const ENDOFTEXT: ControlToken = t(ControlRole::EndOfTurn, "<|endoftext|>");
}

/// Every control token GLM uses, for the token core to resolve once. The order is not
/// load-bearing - lookup is by literal, and a role with several literals returns all of
/// them - it is only the order failures are declared in.
pub const GLM_TOKENS: &[ControlToken] = &[
    tokens::GMASK,
    tokens::SOP,
    tokens::SYSTEM,
    tokens::USER,
    tokens::ASSISTANT,
    tokens::OBSERVATION,
    tokens::THINK_OPEN,
    tokens::THINK_CLOSE,
    tokens::TOOL_CALL_OPEN,
    tokens::TOOL_CALL_CLOSE,
    tokens::TOOL_RESPONSE_OPEN,
    tokens::TOOL_RESPONSE_CLOSE,
    tokens::ARG_KEY_OPEN,
    tokens::ARG_KEY_CLOSE,
    tokens::ARG_VALUE_OPEN,
    tokens::ARG_VALUE_CLOSE,
    tokens::BEGIN_OF_IMAGE,
    tokens::IMAGE,
    tokens::END_OF_IMAGE,
    tokens::ENDOFTEXT,
];

/// GLM stops a turn by starting the next one: `<|user|>` for a reply, `<|observation|>`
/// for a tool call awaiting a result, `<|endoftext|>` for the vocab eos. Each carries its
/// role so a resolution failure says which way of ending a turn stopped working.
pub const GLM_STOP_TOKENS: &[StopToken] = &[
    StopToken::borrowed(ControlRole::EndOfTurn, "<|endoftext|>"),
    StopToken::borrowed(ControlRole::TurnStartUser, "<|user|>"),
    StopToken::borrowed(ControlRole::TurnStartTool, "<|observation|>"),
];

/// Guards watch for a turn that stopped progressing. A repeating-token collapse poisons
/// the slot rather than erroring, so it costs nothing to watch for, and `ReasoningStall`
/// is not a cap on thinking - long thinking is fine - but a stalled-progress signal.
pub const GLM_GUARDS: &[Guard] = &[
    Guard::RepetitionRun { run: 64 },
    Guard::RepetitionNgram {
        window: 32,
        times: 8,
    },
    Guard::ReasoningStall { tokens: 65536 },
];

/// SHA-256 of the shipped jinja, computed from the file rather than pasted next to it, so
/// a model update that changes the template invalidates the spec automatically. It is also
/// the dialect's identity in the store.
pub fn template_sha() -> [u8; 32] {
    static SHA: OnceLock<[u8; 32]> = OnceLock::new();
    *SHA.get_or_init(|| sha256::sha256(TEMPLATE.as_bytes()))
}

/// GLM as data. A function rather than a `const` because `template_sha` hashes the
/// embedded file. `system_update_mode` is `InHistory`: a `system` message renders in place
/// between the surrounding turns, so a mid-session change is appended where it belongs.
pub fn glm_spec() -> DialectSpec {
    DialectSpec {
        name: std::borrow::Cow::Borrowed(NAME),
        template: std::borrow::Cow::Borrowed(TEMPLATE),
        template_sha: template_sha(),
        control_tokens: ControlTokens::borrowed(GLM_TOKENS),
        stop_tokens: GLM_STOP_TOKENS.to_vec(),
        system_update_mode: SystemUpdateMode::InHistory,
        reasoning_field: ReasoningField::ReasoningContent,
        guards: GLM_GUARDS.to_vec(),
    }
}

/// The reasoning-effort line the template emits before the tools block and before message
/// 0, so changing it invalidates the whole prefix. Only `low` and `high` are honoured;
/// anything else, including absent, renders `Max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningEffort {
    Low,
    High,
    #[default]
    Max,
}

impl ReasoningEffort {
    /// What the template's `| capitalize` produces.
    pub fn rendered(self) -> &'static str {
        match self {
            ReasoningEffort::Low => "Low",
            ReasoningEffort::High => "High",
            ReasoningEffort::Max => "Max",
        }
    }

    /// The value to send as OpenAI `reasoning_effort`, or `None` for the default.
    pub fn wire(self) -> Option<&'static str> {
        match self {
            ReasoningEffort::Low => Some("low"),
            ReasoningEffort::High => Some("high"),
            ReasoningEffort::Max => None,
        }
    }
}

/// Hex of a `template_sha`, for logging and for the staleness check.
pub fn sha_hex(sha: &[u8; 32]) -> String {
    sha256::hex(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_sha_is_the_hash_of_the_shipped_jinja() {
        // sha256sum of tokenizer.chat_template extracted from
        // GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf.
        assert_eq!(
            sha_hex(&glm_spec().template_sha),
            "a4fddbbf0b432101a296c17094f8bc5a2b0d30713b5b5cd92f86be78511aa724",
            "the embedded template changed: re-run the fidelity gate before shipping"
        );
    }

    #[test]
    fn every_control_literal_is_unique() {
        // A duplicate literal would make the token core resolve one id twice under two
        // roles and hide the real one.
        let mut seen = std::collections::HashSet::new();
        for t in GLM_TOKENS {
            assert!(seen.insert(&t.literal), "duplicate literal {}", t.literal);
        }
    }

    #[test]
    fn every_glm_token_has_a_real_role() {
        // No GLM token may fall back to `ControlRole::Other`: a bucket role makes
        // `all_with_role` answer a question about some other token, which is how `<sop>` once
        // came back as a turn end.
        let ct = ControlTokens::borrowed(GLM_TOKENS);
        let unroled: Vec<&str> = ct
            .all_with_role(ControlRole::Other)
            .map(|t| t.literal.as_ref())
            .collect();
        assert!(unroled.is_empty(), "no role for {unroled:?}");
        assert_eq!(ct.all_with_role(ControlRole::TurnEnd).count(), 0);

        // And every role GLM declares is answered by exactly one literal, so the
        // ambiguity `all_with_role` exists to expose is absent here -- which is a
        // measurement, not an assumption baked into a lookup.
        for token in ct.iter() {
            assert_eq!(
                ct.all_with_role(token.role).count(),
                1,
                "role {:?} has several literals",
                token.role
            );
        }
    }

    #[test]
    fn lookup_is_by_literal() {
        let ct = ControlTokens::borrowed(GLM_TOKENS);
        assert_eq!(
            ct.by_literal("<|user|>").unwrap().role,
            ControlRole::TurnStartUser
        );
        assert_eq!(
            ct.by_literal("<sop>").unwrap().role,
            ControlRole::SequenceStart
        );
        assert_eq!(
            ct.by_literal("<arg_key>").unwrap().role,
            ControlRole::ArgKeyOpen
        );
        assert!(ct.by_literal("<|im_end|>").is_none());
    }

    #[test]
    fn every_stop_token_is_a_declared_control_token() {
        // A stop literal that is not in the control table is a literal nobody proved
        // is one vocab entry -- and a stop token that is really a sequence never
        // fires, which is the failure that runs a turn to n_ctx.
        let ct = ControlTokens::borrowed(GLM_TOKENS);
        for stop in GLM_STOP_TOKENS {
            let found = ct
                .by_literal(&stop.literal)
                .unwrap_or_else(|| panic!("stop token {} is not in GLM_TOKENS", stop.literal));
            assert_eq!(
                found.role, stop.role,
                "stop token {} disagrees with the control table about its role",
                stop.literal
            );
        }
    }
}
