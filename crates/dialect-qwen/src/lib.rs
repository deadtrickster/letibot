//! Qwen3.8 (Flash-Next and 27B): the spec, a renderer, and a parser.
//!
//! # Read this before you build on it
//!
//! **This is a stopgap, and it is marked as one on purpose.** T1 settled that
//! rendering becomes *template-driven* — the model's own shipped jinja through
//! minijinja, one renderer for every model
//! (`experiments/minijinja-fidelity/RESULTS.md`, D11). A second hand-written
//! renderer is work that decision says we should not do.
//!
//! It exists because M1's exit test is a **live 30-turn session**, the box serves
//! Qwen, and the only renderer that existed was GLM's. The alternative on the table
//! — `crates/turn/tests/support/chatml.rs` — cannot do it, for a reason worth
//! recording: that fixture writes tool calls as JSON inside `<tool_call>…`, and
//! **this model's template does not**. It writes
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
//! so a harness driving Qwen through that fixture would emit a tool-call format the
//! model was never trained on, and would never parse back what it actually emits.
//! The tool loop — the whole point of T17 — is not exercisable without this.
//!
//! **When the template-driven renderer lands, delete this crate.** Keep the
//! [`QwenParser`], which is untouched by T1: a template says how a turn is written
//! and nothing about how to read one back.
//!
//! # Which models this covers
//!
//! Qwen3.8-Flash-Next and Qwen3.8-27B ship a **byte-identical**
//! `tokenizer.chat_template` — sha256 `12827f24b742ea4e…`, 9,993 bytes, verified by
//! extracting from both GGUFs rather than assumed. [`template_sha`] is what proves
//! it: one dialect, two of the plan's three targets.
//!
//! # Four things about this template that are not in the plan
//!
//! **1. A mid-history system message is a hard error, not a silent drop.** The
//! template merges only the *leading* run of system messages and then
//! `raise_exception('System message must be at the beginning.')` on any later one.
//! So Qwen's [`SystemUpdateMode`] is [`Envelope`](SystemUpdateMode::Envelope), and
//! §5.3's conservative default — the update inside a *user* turn with a fixed
//! envelope — is not a caution here, it is the only thing that renders at all.
//! [`system_update_item`] builds that item so that the daemon does not have to know
//! the rule.
//!
//! **2. Reasoning is replayed by default, and that is the fused design's premise
//! holding for free.** `preserve_thinking` is undefined unless a caller sets it, and
//! the template's condition is `preserve_thinking is undefined or … is true or …`,
//! so every assistant turn re-emits its own `<think>` block. Nothing has to be
//! remembered or reconstructed; §5.4's whole category of defect does not arise.
//!
//! **3. The reasoning-effort instruction is prompt bytes at position ~3.** With no
//! `reasoning_effort` the template resolves to `xhigh` and prepends a sentence to
//! the system turn. Switching effort mid-session therefore rewrites the *head* of
//! the prompt and re-prefills everything. [`ReasoningEffort`] is on the renderer for
//! that reason: it is part of the stable prefix, not a sampling knob.
//!
//! **4. Consecutive tool results are merged by the template into one user turn, and
//! we do not merge them.** This is a real, measured divergence and it is declared
//! rather than hidden — see [`QwenRenderer`].
//!
//! # The token table
//!
//! Read out of the GGUF with
//! `python3 tests/fidelity/extract_template.py --tokens <gguf>`, not guessed:
//!
//! ```text
//! 248044 '<|endoftext|>'     CONTROL        248058 '<tool_call>'      USER_DEFINED
//! 248045 '<|im_start|>'      CONTROL        248059 '</tool_call>'     USER_DEFINED
//! 248046 '<|im_end|>'        CONTROL        248066 '<tool_response>'  USER_DEFINED
//! 248053 '<|vision_start|>'  CONTROL        248067 '</tool_response>' USER_DEFINED
//! 248054 '<|vision_end|>'    CONTROL        248068 '<think>'          USER_DEFINED
//! 248056 '<|image_pad|>'     CONTROL        248069 '</think>'         USER_DEFINED
//! ```
//!
//! Note what is **not** in it: `<function=` and `<parameter=` are ordinary text,
//! several tokens each. Emitting them as [`RenderSpan::Control`] would fail to
//! resolve at startup, which is the failure mode the control table exists to force.

use std::sync::OnceLock;

use letibot_dialect::{
    ControlRole, ControlToken, ControlTokens, DialectSpec, Guard, StopToken, SystemUpdateMode,
};
use letibot_transcript::{SystemOrigin, TranscriptItem};

mod json;
mod parse;
mod render;

pub use json::{hf_tojson, parameter_value_text, qwen_tool_json};
pub use parse::{QwenParser, TableDecoder};
pub use render::{QwenRenderer, ends_mid_turn, generation_prompt, outcome_envelope, tools_json};

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
/// `<|im_end|>` is the one the model actually emits; `<|endoftext|>` is the vocab's
/// EOG and is declared so the engine knows the server will halt on it. Both are
/// EOG in this vocabulary, so the engine enforces neither itself — see
/// `TurnEngine::client_stop_ids` for why enforcing an EOG stop client-side throws
/// away the terminal frame and with it the cache numbers.
pub const QWEN_STOP_TOKENS: &[StopToken] = &[
    StopToken::borrowed(ControlRole::TurnEnd, "<|im_end|>"),
    StopToken::borrowed(ControlRole::EndOfTurn, "<|endoftext|>"),
];

/// §8.5's declared mitigations for this model.
///
/// Deliberately loose. A guard that fires on ordinary prose measures the guard, and
/// long thinking is fine — what is not fine is silent non-termination.
pub const QWEN_GUARDS: &[Guard] = &[
    Guard::RepetitionRun { run: 96 },
    Guard::RepetitionNgram {
        window: 32,
        times: 6,
    },
];

/// SHA-256 of [`TEMPLATE`]. Computed once; §4.4 makes this the dialect's identity.
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
        guards: QWEN_GUARDS.to_vec(),
    }
}

/// How much the template tells the model to think.
///
/// These are **prompt bytes near the head of the prefix**, so this is a property of
/// the session, not of a request. `high` is an alias the template folds into
/// `xhigh`; it is not offered here, because offering two spellings of one value
/// invites somebody to switch between them and pay a full re-prefill for nothing.
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

/// §5.3's system update, in the only form this template can render.
///
/// A `System { origin: Update }` item is unrenderable on Qwen — the template raises
/// on it — so a daemon that wants to change the system prompt mid-session appends
/// **this** instead: a user item carrying a fixed envelope. Still an append, still
/// prefix-stable, and it costs the model reading it as user text.
///
/// The sequence number is in the envelope so a head, and the model, can tell a
/// second update from a repeat of the first.
pub fn system_update_item(seq: u64, text: &str) -> TranscriptItem {
    TranscriptItem::User {
        parts: vec![letibot_transcript::UserPart::Text {
            text: format!("<system-update seq={seq}>\n{text}\n</system-update>"),
        }],
    }
}

/// Whether an item is a system update in the above form.
///
/// Used by the daemon to keep §18.1-I6 honest: a system change must be visible as
/// such rather than indistinguishable from something a user typed.
pub fn is_system_update(item: &TranscriptItem) -> bool {
    match item {
        TranscriptItem::System { origin, .. } => *origin == SystemOrigin::Update,
        TranscriptItem::User { parts } => parts.iter().any(|p| {
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
        // Measured by extracting from both GGUFs, 2026-09-09. If a model update
        // changes the template this fails here rather than in a prompt.
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
        assert!(ReasoningEffort::Xhigh.instruction().starts_with("Reasoning effort is set to xhigh."));
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
