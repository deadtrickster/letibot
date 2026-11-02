//! GLM-5.3-Flash: the spec that drives the template renderer, a parser, and a second renderer kept
//! as a cross-check.
//!
//! The primary artefact is [`glm_spec`], a [`DialectSpec`] value: the template, its sha, the
//! control-token table, the stop tokens, the system-update rule and the guards. No code renders it.
//!
//! Three things survive as code. [`GlmParser`], because parsing is not a template: the shipped jinja
//! says how a turn is written and nothing about how to read one back. [`GlmRenderer`], kept as a
//! second independent implementation, because two implementations that agree are stronger evidence
//! than one that is merely tested and it is what the fidelity corpus is measured through.
//! [`check_transcript`], because the shapes that cannot be rendered faithfully are a property of
//! GLM's template rather than of whoever renders it.
//!
//! Four things about GLM that are easy to get wrong:
//!
//! 1. The shipped template leaks reasoning across turns under llama.cpp: a `{%- set %}` inside the
//!    message loop is scoped to the iteration by CPython Jinja2 and kept alive into the next one by
//!    minja, so an assistant turn with no reasoning of its own carries the previous turn's `<think>`.
//!    Identical inputs:
//!
//! ```text
//! server: …<|assistant|><think>R1</think>one<|user|>b<|assistant|><think>R1</think>two…
//! jinja2: …<|assistant|><think>R1</think>one<|user|>b<|assistant|><think></think>two…
//! ```
//!
//!    We render the Jinja2 reading, because that is what Hugging Face runs and therefore what the
//!    model was trained against, and nothing in our path goes through llama.cpp's renderer: we submit
//!    token ids. A template-driven renderer agrees with us for free, and the gate still reports how
//!    the server differs.
//! 2. The shipped template re-sorts tool results into tool-call order, which is incompatible with
//!    append-only rendering: appending a result that sorts before one already rendered would rewrite
//!    bytes already in the KV cache. We render in transcript order and the harness appends in call
//!    order, at which point the sort is a no-op and both properties hold. [`check_transcript`]
//!    reports a transcript that breaks the requirement.
//! 3. Images cannot be checked against `/apply-template` at all: the server replaces image parts with
//!    a media marker carrying a per-process nonce before the template runs, so `emit_image()` never
//!    executes there. We render what the template would have emitted and the gate normalises the
//!    marker on both sides.
//! 4. Every literal in [`GLM_TOKENS`] is one vocab entry, so all of it must be `RenderSpan::Control`.
//!    Half are `USER_DEFINED` rather than `CONTROL` in the GGUF table, so `llama_vocab_is_control`
//!    alone is not the test: accept `CONTROL` or `USER_DEFINED`, reject `NORMAL`.

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

/// The shipped jinja this dialect was validated against, verbatim, extracted from the model's first
/// shard. It is the renderer's input, not documentation.
pub const TEMPLATE: &str = include_str!("../template/glm-5.3-flash.jinja");

/// The name this dialect is known by, in the gate and in the store.
pub const NAME: &str = "glm-5.3-flash";

/// The literal spelling of every GLM token this dialect emits, each a single vocab entry, which is why
/// they are emitted as `RenderSpan::Control` and never as text.
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

/// Every control token GLM uses, for the token core to resolve once. The order only decides where a
/// failure is reported: lookup is by literal, and a role with several literals returns all of them.
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

/// What ends a generation: `<|user|>` and `<|observation|>` are the two ways a turn legitimately ends
/// and `<|endoftext|>` is the vocab eos. Each carries its role, so a resolve failure names which one.
pub const GLM_STOP_TOKENS: &[StopToken] = &[
    StopToken::borrowed(ControlRole::EndOfTurn, "<|endoftext|>"),
    StopToken::borrowed(ControlRole::TurnStartUser, "<|user|>"),
    StopToken::borrowed(ControlRole::TurnStartTool, "<|observation|>"),
];

/// The declared mitigations. A repeating-token collapse poisons a slot rather than erroring and has
/// never been reached here, so it costs nothing to watch for; `ReasoningStall` is not a cap on
/// thinking, it is a signal that the turn stopped progressing.
pub const GLM_GUARDS: &[Guard] = &[
    Guard::RepetitionRun { run: 64 },
    Guard::RepetitionNgram {
        window: 32,
        times: 8,
    },
    Guard::ReasoningStall { tokens: 65536 },
];

/// SHA-256 of the shipped jinja, computed from the file rather than pasted beside it, so a model
/// update that changes the template invalidates the spec. This is the dialect's identity in the store.
pub fn template_sha() -> [u8; 32] {
    static SHA: OnceLock<[u8; 32]> = OnceLock::new();
    *SHA.get_or_init(|| sha256::sha256(TEMPLATE.as_bytes()))
}

/// GLM as data. A function rather than a `const` because `template_sha` hashes the embedded file and
/// hashing is not a `const fn`; `system_update_mode` is `InHistory`, since a `system` message at index
/// 2 renders in place and a mid-session change costs only its own tokens.
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

/// The reasoning-effort line the template emits near the head of every prompt: changing it
/// invalidates the whole prefix. Only `low` and `high` are honoured, and absent renders `Max`.
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
        // sha256sum of the template extracted from the model's first shard. A change means re-run
        // the fidelity gate before shipping.
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
        // The GAP-2 regression test: eight of these were once declared `TurnEnd`, a role GLM does not
        // use, purely because the enum had no variant for them. This keeps that bucket from coming back.
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
