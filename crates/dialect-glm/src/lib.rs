//! GLM-5.3-Flash: the spec that drives the template renderer, a parser, and a
//! second renderer kept as a cross-check.
//!
//! # What this crate is, after T1
//!
//! T1 settled that prompts are rendered by running the model's **own shipped jinja**
//! through a Rust Jinja engine, not by a renderer we wrote per model
//! (`experiments/minijinja-fidelity/RESULTS.md`, D11). So the primary artefact here is
//! now [`glm_spec`] — a [`DialectSpec`] value: the template, its sha, the control-token
//! table, the stop tokens, the system-update rule and the guards. No code renders it.
//!
//! Three things survive as code, and each has a reason:
//!
//! * [`GlmParser`] — parsing is not a template. The shipped jinja says how a turn is
//!   *written* and nothing at all about how to read one back.
//! * [`GlmRenderer`] — **demoted, deliberately kept.** It is a second, independent
//!   implementation of the same function. Two implementations that agree are stronger
//!   evidence than one that is merely tested, and it is what the 139-case fidelity
//!   corpus is currently measured through. T1's own recommendation was to keep the
//!   differential, not to delete the loser.
//! * [`check_transcript`] — the transcript shapes that cannot be rendered faithfully
//!   are a property of GLM's template, not of whoever renders it, so they outlive the
//!   renderer that first reported them.
//!
//! # Four things about GLM that are not in the plan
//!
//! **1. The shipped template leaks reasoning across turns — under llama.cpp only.**
//! `{%- set reasoning_content = m.reasoning_content %}` sits inside the `{% for m in
//! messages %}` body. CPython Jinja2 scopes that to the iteration; llama.cpp's minja
//! keeps it alive into the *next* iteration. So an assistant turn with no reasoning of
//! its own is rendered by the server carrying the **previous** turn's `<think>` block.
//! Measured 2026-09-09, identical inputs:
//!
//! ```text
//! server: …<|assistant|><think>R1</think>one<|user|>b<|assistant|><think>R1</think>two…
//! jinja2: …<|assistant|><think>R1</think>one<|user|>b<|assistant|><think></think>two…
//! ```
//!
//! We render the **Jinja2** reading, because Jinja2 is what Hugging Face runs and
//! therefore what the model was trained against, and because nothing in our path goes
//! through llama.cpp's renderer — we submit token ids. T1 confirmed minijinja does not
//! have the bug either, so the template-driven renderer agrees with us for free.
//!
//! There used to be a `server-bug-compatible` profile that reproduced the leak, to
//! prove we understood the template well enough to reproduce minja exactly. **It is
//! gone** (T2): a template-driven renderer cannot produce it without deliberately
//! reintroducing someone else's bug, and running the real template through a correct
//! engine is stronger evidence than reproducing a wrong one. The gate's INTEROP phase
//! still reports how the server differs; `reasoning-leak.json` still declares the
//! divergence and is still rendered against the authority.
//!
//! **2. Tool results are re-sorted by the shipped template, and that is incompatible
//! with append-only rendering.** When every `tool` message in a block carries a unique
//! id matching one of the preceding assistant's `tool_calls`, the template emits them
//! **in tool-call order**, not message order. Appending a result that sorts before one
//! already rendered would rewrite bytes that are already in the KV cache. We therefore
//! render results in **transcript order** and require the harness to append them in
//! call order — at which point the template's sort is a no-op and both properties hold
//! at once. [`check_transcript`] reports a transcript that breaks the requirement
//! instead of silently diverging, and it does so for the template-driven renderer too:
//! the sort is the template's, so the hazard is the template's.
//!
//! **3. Images cannot be checked against `/apply-template` at all.** llama.cpp replaces
//! image parts with a media marker containing a **per-process random nonce**
//! (`<__media_SXS4xEDCuyaeIR7cBV6jKB8RhPA841Ns__>`) before the template runs, so
//! `emit_image()` never executes on the server path. We render what the template would
//! have emitted — `<|begin_of_image|><|image|><|end_of_image|>` — and the gate
//! normalises the marker on both sides. Nothing stronger is available.
//!
//! **4. `[gMASK]<sop>` is two tokens, `<arg_key>`/`<arg_value>` are single tokens.**
//! Everything listed in [`GLM_TOKENS`] is one vocab entry, so all of it must be
//! `RenderSpan::Control` — emitting `<arg_key>` as `Text` would tokenize it as six
//! ordinary tokens and diverge from training. Under T2 every one of them now has an
//! honest [`ControlRole`]; they used to be declared as `TurnEnd`, a role GLM does not
//! use, as an unnamed "no role" bucket.
//!
//! Half of them are `USER_DEFINED`, not `CONTROL`, in the GGUF token table (`<think>`,
//! `</think>`, `<tool_call>`, `</tool_call>`, `<tool_response>`, `</tool_response>`,
//! `<arg_key>`, `</arg_key>`, `<arg_value>`, `</arg_value>` are ids 154841–154850,
//! attribute `USER_DEFINED`; the `<|…|>` family and `[gMASK]`, `<sop>` are `CONTROL`).
//! So `llama_vocab_is_control` alone is **not** the test for "may this literal be
//! resolved as a control token" — it would reject exactly half of this dialect. Accept
//! `CONTROL` or `USER_DEFINED`, reject `NORMAL`.
//! `tests/fidelity/extract_template.py --tokens <gguf>` prints the table.
//!
//! # What happened to CONTRACT-GAPS
//!
//! The three gaps this crate used to carry are closed rather than worked around:
//!
//! - **GAP-1** (`render_incremental` cannot be pure) — gone twice over. The contract no
//!   longer has the method (Jinja has no incremental mode), and [`GlmRenderer`]'s own
//!   inherent version takes the history explicitly instead of being handed it through
//!   a builder that panicked when it was missing.
//! - **GAP-2** (`ControlRole` too small) — closed. Every GLM token has a real role.
//! - **GAP-3** (`parse` has no vocab) — closed. [`letibot_dialect::TokenDecoder`] is in
//!   the contract, so `parse` takes one as an argument instead of a builder storing one
//!   and a panic if it was forgotten.
//!
//! See `docs/implementation-plan.md` §7 and `tests/fidelity/README.md`.

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
    Anomaly, GlmRenderer, check_transcript, ends_mid_turn, generation_prompt, outcome_envelope,
};

/// The shipped jinja this dialect was validated against, verbatim.
///
/// Extracted with:
/// `python3 tests/fidelity/extract_template.py <model-00001-of-00006.gguf>`
///
/// Under T1 this is no longer documentation — it is the renderer's input.
pub const TEMPLATE: &str = include_str!("../template/glm-5.3-flash.jinja");

/// The name this dialect is known by, in the gate and in the store.
pub const NAME: &str = "glm-5.3-flash";

/// The literal spelling of every GLM token this dialect emits.
///
/// Each of these is a **single vocab entry** (verified against the GGUF token table:
/// ids 154820–154855, `CONTROL` or `USER_DEFINED`). That is why they are emitted as
/// `RenderSpan::Control` and never as text.
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

/// Every control token GLM uses, for the token core to resolve once.
///
/// The order used to be load-bearing — `ControlTokens::get(role)` returned the first
/// match, so the canonical entry had to come first. It is not any more: lookup is by
/// literal, and a role with several literals returns all of them. This order is now
/// only the order failures are declared in, and the report sorts anyway.
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

/// GLM stops a turn by starting the next one.
///
/// `<|user|>` and `<|observation|>` are the two ways a generation legitimately ends (a
/// reply, or a tool call awaiting a result); `<|endoftext|>` is the vocab eos. Each
/// carries the role it plays, so a resolution failure can say *which* of the three
/// ways to end a turn stopped working rather than printing a literal and leaving the
/// reader to guess what it cost.
///
/// From the GGUF: `eos=154820 <|endoftext|>`, `eot=154827 <|user|>`,
/// `eom=154829 <|observation|>`.
pub const GLM_STOP_TOKENS: &[StopToken] = &[
    StopToken::borrowed(ControlRole::EndOfTurn, "<|endoftext|>"),
    StopToken::borrowed(ControlRole::TurnStartUser, "<|user|>"),
    StopToken::borrowed(ControlRole::TurnStartTool, "<|observation|>"),
];

/// §7.3: the repeating-token collapse past ~78k at `-ub 512` poisons the slot rather
/// than erroring, and does not reproduce on this build (probed to 147,042 tokens). It
/// costs nothing to watch for, and 1M has never been run. `ReasoningStall` is not a cap
/// on thinking — long thinking is fine — it is a signal that the turn stopped
/// progressing.
pub const GLM_GUARDS: &[Guard] = &[
    Guard::RepetitionRun { run: 64 },
    Guard::RepetitionNgram {
        window: 32,
        times: 8,
    },
    Guard::ReasoningStall { tokens: 65536 },
];

/// SHA-256 of the shipped jinja.
///
/// Computed from the file rather than pasted next to it, so a model update that
/// changes the template invalidates the spec automatically. §4.4 makes this the
/// dialect's identity in the store.
pub fn template_sha() -> [u8; 32] {
    static SHA: OnceLock<[u8; 32]> = OnceLock::new();
    *SHA.get_or_init(|| sha256::sha256(TEMPLATE.as_bytes()))
}

/// GLM as data.
///
/// A function rather than a `const` for one honest reason: `template_sha` is a hash of
/// the embedded file, and hashing is not a `const fn` here. Everything else in it
/// borrows, so the only cost is the two small `Vec`s.
///
/// GLM's `system_update_mode` is `InHistory`, verified: a `system` message at index 2
/// renders as `<|system|>…` in place, between the surrounding turns. So a mid-session
/// prompt change is appended, and costs nothing beyond its own tokens.
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

/// The reasoning-effort line the template emits at position 3 of every prompt.
///
/// It is `<|system|>Reasoning Effort: {Low|High|Max}` and it sits **before** the
/// tools block and before message 0, so changing it invalidates the entire prefix.
/// That is not a quirk of ours: `effective_reasoning_effort` is only honoured for
/// `low` and `high`, and anything else — including absent — renders `Max`.
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
        // GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf, 2026-09-09.
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
        // The GAP-2 regression test. Eight of these used to be declared `TurnEnd`,
        // which GLM does not use, purely because the enum had no variant for them --
        // so `get(TurnEnd)` answered a question about `<sop>`. Both the bucket and the
        // lookup that made it dangerous are gone; this keeps the bucket from coming
        // back under its new name.
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
