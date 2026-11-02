//! Qwen3.8 (Flash-Next and 27B): the spec, a renderer, and a parser.
//!
//! The renderer is hand-written, which the template-driven design says we should not do.
//! It exists because that renderer is not here yet, and this model's template writes a tool call as
//! `<function=name>` / `<parameter=name>` text under `<tool_call>` rather than as JSON:
//!
//! ```text
//! <tool_call>
//! <function=read>
//! <parameter=path>
//! src/main.rs
//! </parameter>
//! </function>
//! </tool_call>
//! ```
//!
//! so the ChatML fixture cannot drive it: the model would never parse back what it emits. Keep the
//! [`QwenParser`] when the template-driven renderer lands; a template says how a turn is written and
//! nothing about how to read one back.
//!
//! One dialect covers two models: Qwen3.8-Flash-Next and Qwen3.8-27B ship a byte-identical
//! `tokenizer.chat_template`, checked by extracting from both GGUFs, and [`template_sha`] is what
//! proves it.
//!
//! Four things about this template that are easy to get wrong:
//!
//! 1. A mid-history system message is a hard error: the template merges only the leading run of
//!    system messages and raises on any later one. So [`SystemUpdateMode::Envelope`] is the only
//!    thing that renders, and [`system_update_item`] builds that item so the daemon need not know.
//! 2. Reasoning is replayed by default: `preserve_thinking` is undefined unless a caller sets it,
//!    so every assistant turn re-emits its own `<think>` block and nothing has to be reconstructed.
//! 3. The reasoning-effort instruction is prompt bytes near the head, so switching effort
//!    mid-session rewrites the head and re-prefills. [`ReasoningEffort`] sits on the renderer
//!    because it is part of the stable prefix, not a sampling knob.
//! 4. The template merges consecutive tool results into one user turn and we emit one turn each.
//!    That divergence is declared rather than hidden; see [`QwenRenderer`].
//!
//! `<function=` and `<parameter=` are ordinary text and several tokens each, so declaring them as
//! control tokens would fail to resolve at startup. That failure is what the control table is for:
//!
//! ```text
//! 248044 '<|endoftext|>'     CONTROL        248058 '<tool_call>'      USER_DEFINED
//! 248045 '<|im_start|>'      CONTROL        248059 '</tool_call>'     USER_DEFINED
//! 248046 '<|im_end|>'        CONTROL        248066 '<tool_response>'  USER_DEFINED
//! 248053 '<|vision_start|>'  CONTROL        248067 '</tool_response>' USER_DEFINED
//! 248054 '<|vision_end|>'    CONTROL        248068 '<think>'          USER_DEFINED
//! 248056 '<|image_pad|>'     CONTROL        248069 '</think>'         USER_DEFINED
//! ```

pub mod cli;
use std::sync::OnceLock;

use letibot_dialect::{
    ControlRole, ControlToken, ControlTokens, DialectSpec, Guard, ReasoningField, StopToken,
    SystemUpdateMode,
};
use letibot_transcript::{SystemOrigin, TranscriptItem};

mod json;
mod parse;
mod render;

pub use json::{hf_tojson, parameter_value_text, qwen_tool_json};
pub use parse::{QwenParser, TableDecoder};
pub use render::{
    QwenRenderer, ends_mid_turn, generation_prompt, generation_prompt_closing_reasoning,
    outcome_envelope, tools_json,
};

/// The shipped jinja this dialect renders, verbatim, extracted with
/// `python3 tests/fidelity/extract_template.py <model-00001-of-00006.gguf>`.
pub const TEMPLATE: &str = include_str!("../template/qwen3.8-flash-next.jinja");

/// The name this dialect is known by, in the gate and in the store.
///
/// The *dialect* name, not a model name: `qwen-3.8-flash-next` and `qwen3.8-27b`
/// are two models that share it.
pub const NAME: &str = "qwen3.8";

/// The literal spelling of every Qwen token this dialect emits.
///
/// Each is a **single vocab entry**. That is why they are `RenderSpan::Control` and
/// never text.
pub mod tokens {
    use letibot_dialect::{ControlRole, ControlToken};

    const fn t(role: ControlRole, literal: &'static str) -> ControlToken {
        ControlToken::borrowed(role, literal)
    }

    /// Qwen writes the role as *text* after this token (`<|im_start|>user\n`), so
    /// there is no per-role turn-start token to declare. `Other` says that honestly
    /// instead of picking one of the `TurnStart*` roles and being wrong for the
    /// other three.
    pub const IM_START: ControlToken = t(ControlRole::Other, "<|im_start|>");
    pub const IM_END: ControlToken = t(ControlRole::TurnEnd, "<|im_end|>");
    pub const ENDOFTEXT: ControlToken = t(ControlRole::EndOfTurn, "<|endoftext|>");
    pub const THINK_OPEN: ControlToken = t(ControlRole::ThinkOpen, "<think>");
    pub const THINK_CLOSE: ControlToken = t(ControlRole::ThinkClose, "</think>");
    pub const TOOL_CALL_OPEN: ControlToken = t(ControlRole::ToolCallOpen, "<tool_call>");
    pub const TOOL_CALL_CLOSE: ControlToken = t(ControlRole::ToolCallClose, "</tool_call>");
    pub const TOOL_RESPONSE_OPEN: ControlToken = t(ControlRole::ToolResultOpen, "<tool_response>");
    pub const TOOL_RESPONSE_CLOSE: ControlToken =
        t(ControlRole::ToolResultClose, "</tool_response>");
    pub const VISION_START: ControlToken = t(ControlRole::ImageOpen, "<|vision_start|>");
    pub const IMAGE_PAD: ControlToken = t(ControlRole::Image, "<|image_pad|>");
    pub const VISION_END: ControlToken = t(ControlRole::ImageClose, "<|vision_end|>");
}

/// The control table, in the order the resolver reports failures in.
pub const QWEN_TOKENS: &[ControlToken] = &[
    tokens::IM_START,
    tokens::IM_END,
    tokens::ENDOFTEXT,
    tokens::THINK_OPEN,
    tokens::THINK_CLOSE,
    tokens::TOOL_CALL_OPEN,
    tokens::TOOL_CALL_CLOSE,
    tokens::TOOL_RESPONSE_OPEN,
    tokens::TOOL_RESPONSE_CLOSE,
    tokens::VISION_START,
    tokens::IMAGE_PAD,
    tokens::VISION_END,
];

/// What ends a turn.
///
/// `<|im_end|>` is the one the model actually emits; `<|endoftext|>` is the vocab's EOG and is
/// declared so the engine knows the server will halt on it. Both are EOG, so the engine enforces
/// neither itself: stopping an EOG client-side throws away the terminal frame and its cache numbers.
pub const QWEN_STOP_TOKENS: &[StopToken] = &[
    StopToken::borrowed(ControlRole::TurnEnd, "<|im_end|>"),
    StopToken::borrowed(ControlRole::EndOfTurn, "<|endoftext|>"),
];

/// The declared mitigations for this model.
///
/// Deliberately loose: a guard that fires on ordinary prose measures the guard, and long thinking
/// is fine. Silent non-termination is not.
pub const QWEN_GUARDS: &[Guard] = &[
    Guard::RepetitionRun { run: 96 },
    Guard::RepetitionNgram {
        window: 32,
        times: 6,
    },
];

/// SHA-256 of [`TEMPLATE`]. Computed once, and it is the dialect's identity in the store.
pub fn template_sha() -> [u8; 32] {
    static SHA: OnceLock<[u8; 32]> = OnceLock::new();
    *SHA.get_or_init(|| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(TEMPLATE.as_bytes());
        h.finalize().into()
    })
}

pub fn sha_hex(sha: &[u8; 32]) -> String {
    sha.iter().map(|b| format!("{b:02x}")).collect()
}

/// The dialect, as a value.
pub fn qwen_spec() -> DialectSpec {
    DialectSpec {
        name: std::borrow::Cow::Borrowed(NAME),
        template: std::borrow::Cow::Borrowed(TEMPLATE),
        template_sha: template_sha(),
        control_tokens: ControlTokens::borrowed(QWEN_TOKENS),
        stop_tokens: QWEN_STOP_TOKENS.to_vec(),
        // Not a preference. The template raises on a system message that is not in
        // the leading run, so `InHistory` is not renderable at all here.
        system_update_mode: SystemUpdateMode::Envelope,
        reasoning_field: ReasoningField::Inline,
        guards: QWEN_GUARDS.to_vec(),
    }
}

/// How much the template tells the model to think.
///
/// Prompt bytes near the head of the prefix, so this is a property of the session rather than of a
/// request. `high` is an alias the template folds into `xhigh` and is not offered here: two
/// spellings of one value invite a switch that pays a full re-prefill for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningEffort {
    /// The template's own default when `reasoning_effort` is unset.
    #[default]
    Xhigh,
    /// The one value that emits no instruction sentence at all.
    Medium,
    Low,
}

impl ReasoningEffort {
    /// The sentence the template prepends to the system turn. Empty for `medium`.
    pub fn instruction(self) -> &'static str {
        match self {
            ReasoningEffort::Xhigh => {
                "Reasoning effort is set to xhigh. Please think carefully through the task, \
                 validate key assumptions, consider plausible alternatives, and prioritize \
                 correctness, consistency, and clarity in the final answer."
            }
            ReasoningEffort::Medium => "",
            ReasoningEffort::Low => {
                "Reasoning effort is set to low. Keep your thinking brief and focused, moving \
                 directly to the conclusion without unnecessary elaboration."
            }
        }
    }

    /// The `reasoning_effort` value to put on the wire for the fidelity oracle.
    /// `None` for the template's own default, so the corpus exercises the
    /// unset path as well as the explicit ones.
    pub fn wire(self) -> Option<&'static str> {
        match self {
            ReasoningEffort::Xhigh => None,
            ReasoningEffort::Medium => Some("medium"),
            ReasoningEffort::Low => Some("low"),
        }
    }

    pub fn parse(s: &str) -> Option<ReasoningEffort> {
        match s {
            "xhigh" | "high" => Some(ReasoningEffort::Xhigh),
            "medium" => Some(ReasoningEffort::Medium),
            "low" => Some(ReasoningEffort::Low),
            _ => None,
        }
    }
}

/// The system update, in the only form this template can render.
///
/// A `System { origin: Update }` item is unrenderable on Qwen, so a daemon changing the system
/// prompt mid-session appends this instead: a user item carrying a fixed envelope. Still an append
/// and still prefix-stable, at the cost of the model reading it as user text.
///
/// The sequence number is in the envelope so a reader can tell a second update from a repeat.
pub fn system_update_item(seq: u64, text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![letibot_transcript::UserPart::Text {
            text: format!("<system-update seq={seq}>\n{text}\n</system-update>"),
        }],
    }
}

/// Whether an item is a system update in the above form.
///
/// A system change must be visible as such rather than indistinguishable from something a user
/// typed.
pub fn is_system_update(item: &TranscriptItem) -> bool {
    match item {
        TranscriptItem::System { origin, .. } => *origin == SystemOrigin::Update,
        TranscriptItem::User { parts, .. } => parts.iter().any(|p| {
            matches!(p, letibot_transcript::UserPart::Text { text }
                if text.starts_with("<system-update seq="))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_the_one_both_qwen_targets_ship() {
        // If a model update changes the template this fails here rather than in a prompt.
        assert_eq!(
            sha_hex(&template_sha()),
            "12827f24b742ea4e80cdc12dbcf9622227056b9f797252a3149263d4f9aaadce"
        );
        assert_eq!(TEMPLATE.len(), 9993);
    }

    #[test]
    fn a_mid_history_system_change_is_not_a_system_message_here() {
        // The template raises on one. Flipping this to InHistory makes the renders stop matching
        // the training runtime, silently, at turn N.
        assert_eq!(qwen_spec().system_update_mode, SystemUpdateMode::Envelope);
        assert!(is_system_update(&system_update_item(2, "now in Russian")));
    }

    #[test]
    fn medium_is_the_effort_that_says_nothing() {
        assert_eq!(ReasoningEffort::Medium.instruction(), "");
        assert!(
            ReasoningEffort::Xhigh
                .instruction()
                .starts_with("Reasoning effort is set to xhigh.")
        );
        assert_eq!(ReasoningEffort::default(), ReasoningEffort::Xhigh);
    }

    #[test]
    fn function_and_parameter_are_not_control_tokens() {
        // They are text in the template and several tokens in the vocabulary.
        // Declaring one would fail to resolve at startup, loudly, which is right —
        // but it is cheaper to not declare it.
        let ct = ControlTokens::borrowed(QWEN_TOKENS);
        assert!(ct.by_literal("<function=").is_none());
        assert!(ct.by_literal("<parameter=").is_none());
        assert!(ct.by_literal("<tool_call>").is_some());
    }
}
