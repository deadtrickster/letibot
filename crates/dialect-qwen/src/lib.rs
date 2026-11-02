//! Qwen3.8 (Flash-Next and 27B): the spec, a renderer, and a parser.
//!
//! The two models ship a byte-identical `tokenizer.chat_template`, so they are one dialect and
//! [`template_sha`] proves it. The renderer is hand-written because the template-driven one is not
//! wired for this model yet; the parser outlives it, since a template says how a turn is written
//! and nothing about reading one back. A mid-history system message is a hard error here, so
//! `SystemUpdateMode::Envelope` is the only thing that renders, reasoning is replayed by default,
//! and a tool call is XML rather than JSON, which no ChatML fixture can drive.

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
/// `python3 tests/fidelity/extract_template.py <model-00001-of-00006.gguf>`. It writes a tool call
/// as this and not as JSON:
/// ```text
/// <tool_call>
/// <function=read>
/// <parameter=path>
/// src/main.rs
/// </parameter>
/// </function>
/// </tool_call>
/// ```
pub const TEMPLATE: &str = include_str!("../template/qwen3.8-flash-next.jinja");

/// The name this dialect is known by, in the gate and in the store. It is the dialect name,
/// not a model name: `qwen-3.8-flash-next` and `qwen3.8-27b` are two models that share it.
pub const NAME: &str = "qwen3.8";

/// The literal spelling of every Qwen token this dialect emits. Each is a single vocab entry,
/// which is why it is a `RenderSpan::Control` and never text. `<function=` and `<parameter=`
/// are ordinary text here, several tokens each, so declaring one would fail to resolve at startup.
pub mod tokens {
    use letibot_dialect::{ControlRole, ControlToken};

    const fn t(role: ControlRole, literal: &'static str) -> ControlToken {
        ControlToken::borrowed(role, literal)
    }

    /// Qwen writes the role as text after this token (`<|im_start|>user\n`), so there is no
    /// per-role turn-start token to declare; `Other` says that honestly instead of picking one
    /// of the `TurnStart*` roles and being wrong for the other three.
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

/// The control table, in the order the resolver reports failures in. Read out of the GGUF
/// with `tests/fidelity/extract_template.py --tokens <gguf>`, not guessed:
/// ```text
/// 248044 '<|endoftext|>'     CONTROL        248058 '<tool_call>'      USER_DEFINED
/// 248045 '<|im_start|>'      CONTROL        248059 '</tool_call>'     USER_DEFINED
/// 248046 '<|im_end|>'        CONTROL        248066 '<tool_response>'  USER_DEFINED
/// 248053 '<|vision_start|>'  CONTROL        248067 '</tool_response>' USER_DEFINED
/// 248054 '<|vision_end|>'    CONTROL        248068 '<think>'          USER_DEFINED
/// 248056 '<|image_pad|>'     CONTROL        248069 '</think>'         USER_DEFINED
/// ```
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

/// What ends a turn: `<|im_end|>` is the one the model emits, `<|endoftext|>` the vocab's EOG,
/// declared so the engine knows the server halts on it. Both are EOG here, so the engine
/// enforces neither itself - a client-side EOG stop loses the terminal frame and its cache numbers.
pub const QWEN_STOP_TOKENS: &[StopToken] = &[
    StopToken::borrowed(ControlRole::TurnEnd, "<|im_end|>"),
    StopToken::borrowed(ControlRole::EndOfTurn, "<|endoftext|>"),
];

/// The guards for this model, deliberately loose: a guard that fires on ordinary prose
/// measures the guard, and long thinking is fine - silent non-termination is not.
pub const QWEN_GUARDS: &[Guard] = &[
    Guard::RepetitionRun { run: 96 },
    Guard::RepetitionNgram {
        window: 32,
        times: 6,
    },
];

/// SHA-256 of [`TEMPLATE`], computed once: it is the dialect's identity in the store.
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

/// How much the template tells the model to think: prompt bytes near the head of the prefix, so a
/// property of the session, not of a request. `high` is an alias the template folds into `xhigh`
/// and is not offered, since two spellings of one value invite a re-prefill for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningEffort {
    /// The template's own default when `reasoning_effort` is unset.
    #[default]
    Xhigh,
    /// The one value that emits **no** instruction sentence at all.
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

/// The system update, in the only form this template can render: a `System { origin: Update }`
/// item raises on Qwen, so a mid-session change appends a user item carrying this envelope - still
/// prefix-stable - and the sequence number tells a second update from a repeat of the first.
pub fn system_update_item(seq: u64, text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![letibot_transcript::UserPart::Text {
            text: format!("<system-update seq={seq}>\n{text}\n</system-update>"),
        }],
    }
}

/// Whether an item is a system update in the above form. The daemon uses it to keep a system
/// change visible as such rather than indistinguishable from something a user typed.
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
        // Measured by extracting from both GGUFs. If a model update changes the
        // template this fails here rather than in a prompt.
        assert_eq!(
            sha_hex(&template_sha()),
            "12827f24b742ea4e80cdc12dbcf9622227056b9f797252a3149263d4f9aaadce"
        );
        assert_eq!(TEMPLATE.len(), 9993);
    }

    #[test]
    fn a_mid_history_system_change_is_not_a_system_message_here() {
        // The template raises on one. If somebody ever flips this to InHistory the
        // renders stop matching the training runtime, silently, at turn N.
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
        // Declaring one would fail to resolve at startup, loudly, which is right -
        // but it is cheaper to not declare it.
        let ct = ControlTokens::borrowed(QWEN_TOKENS);
        assert!(ct.by_literal("<function=").is_none());
        assert!(ct.by_literal("<parameter=").is_none());
        assert!(ct.by_literal("<tool_call>").is_some());
    }

    #[test]
    fn every_stop_token_is_a_declared_control_token() {
        // A stop literal missing from the control table is a literal nobody proved is one vocab
        // entry, and a stop token that is really a sequence never fires -- the turn runs to
        // `n_ctx` with nothing saying why. `resolve_stops` reports the failure by the role.
        let ct = ControlTokens::borrowed(QWEN_TOKENS);
        for stop in QWEN_STOP_TOKENS {
            let found = ct
                .by_literal(&stop.literal)
                .unwrap_or_else(|| panic!("stop token {} is not in QWEN_TOKENS", stop.literal));
            assert_eq!(
                found.role, stop.role,
                "stop token {} disagrees with the control table about its role",
                stop.literal
            );
        }
    }

    #[test]
    fn the_image_triplet_has_three_roles_and_media_reads_them() {
        // The three roles are load-bearing, not labels: `media_spans` counts `ImageOpen` and
        // `spans_to_string_with_media` places one marker there while dropping the pad and the
        // close, so a triplet declared as three `Other`s carries no picture or three copies.
        use letibot_dialect::{
            RenderSpan, media_spans, spans_to_string, spans_to_string_with_media,
        };

        let ct = ControlTokens::borrowed(QWEN_TOKENS);
        assert_eq!(
            ct.by_literal("<|vision_start|>").unwrap().role,
            ControlRole::ImageOpen
        );
        assert_eq!(
            ct.by_literal("<|image_pad|>").unwrap().role,
            ControlRole::Image
        );
        assert_eq!(
            ct.by_literal("<|vision_end|>").unwrap().role,
            ControlRole::ImageClose
        );

        let spans: Vec<RenderSpan> = [
            "before",
            "<|vision_start|>",
            "<|image_pad|>",
            "<|vision_end|>",
            "after",
        ]
        .iter()
        .map(|lit| match ct.by_literal(lit) {
            Some(t) => RenderSpan::Control(t.clone()),
            None => RenderSpan::Text((*lit).to_string()),
        })
        .collect();

        assert_eq!(media_spans(&spans), 1);
        assert_eq!(
            spans_to_string_with_media(&spans, "<IMG>"),
            "before<IMG>after"
        );
        assert_eq!(
            spans_to_string(&spans),
            "before<|vision_start|><|image_pad|><|vision_end|>after"
        );
    }

    #[test]
    fn im_start_is_other_on_purpose() {
        // Qwen writes the role as text after this token (`<|im_start|>user\n`), so no per-role
        // turn-start token exists to declare. `Other` is what this contract calls a role it has
        // not modelled, and it is greppable: a sweep fixing every `Other` must not touch this.
        assert_eq!(tokens::IM_START.role, ControlRole::Other);
    }
}
