//! The GLM-5.3-Flash dialect: a renderer and parser we own.
//!
//! # What this crate is
//!
//! A pure function. No vocab, no tokenizer, no GPU, no server — `render` turns a
//! [`StablePrefix`] plus a slice of [`TranscriptItem`]s into `[Text | Control]`
//! spans, and `letibot_dialect::spans_to_string` turns those into the exact string
//! `POST /apply-template` returns for the same conversation. That equality is the
//! W3 gate (`tests/fidelity/`), and it is the only thing that makes "we own the
//! renderer" safe to say.
//!
//! Everything here was derived by probing a live `/apply-template` against the
//! shipped jinja, not by reading the template and hoping. The template itself is
//! embedded at `template/glm-5.3-flash.jinja`, extracted from
//! `GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf` (`tokenizer.chat_template`), and
//! [`GlmDialect::template_sha`] is computed from that file rather than pasted next
//! to it.
//!
//! # Four things about GLM that are not in the plan
//!
//! **1. The shipped template leaks reasoning across turns — under llama.cpp only.**
//! `{%- set reasoning_content = m.reasoning_content %}` sits inside the `{% for m in
//! messages %}` body. CPython Jinja2 scopes that to the iteration; llama.cpp's jinja
//! keeps it alive into the *next* iteration. So an assistant turn with no reasoning
//! of its own is rendered by the server carrying the **previous** turn's `<think>`
//! block. Measured 2026-09-09, identical inputs:
//!
//! ```text
//! server: …<|assistant|><think>R1</think>one<|user|>b<|assistant|><think>R1</think>two…
//! jinja2: …<|assistant|><think>R1</think>one<|user|>b<|assistant|><think></think>two…
//! ```
//!
//! It fires in production, not just in theory: llama.cpp drops `reasoning_content`
//! when it is the empty string, so a genuinely empty think block is indistinguishable
//! from an absent one and inherits the older text.
//!
//! This dialect renders the **Jinja2** reading by default, because Jinja2 is what
//! Hugging Face runs and therefore what the model was trained against, and because
//! nothing in our path goes through llama.cpp's renderer — we submit token ids. The
//! divergence is declared to the fidelity gate rather than hidden, and
//! [`GlmQuirks::reasoning_leak`] reproduces the server's behaviour exactly, so the
//! gate can prove our renderer is a *complete* model of the oracle before excusing
//! the one place it deliberately differs. Flip the default in one line if the call
//! goes the other way.
//!
//! **2. Tool results are re-sorted by the shipped template, and that is
//! incompatible with append-only rendering.** When every `tool` message in a block
//! carries a unique id matching one of the preceding assistant's `tool_calls`, the
//! template emits them **in tool-call order**, not message order. Appending a result
//! that sorts before one already rendered would rewrite bytes that are already in the
//! KV cache, so `render_incremental ≡ render` cannot hold if we copy that. We
//! therefore render results in **transcript order** and require the harness to append
//! them in call order — at which point the template's sort is a no-op and both
//! properties hold at once. [`GlmDialect::check_transcript`] reports a transcript
//! that breaks the requirement instead of silently diverging.
//!
//! **3. Images cannot be checked against `/apply-template` at all.** llama.cpp
//! replaces image parts with a media marker containing a **per-process random
//! nonce** (`<__media_SXS4xEDCuyaeIR7cBV6jKB8RhPA841Ns__>`) before the template runs,
//! so `emit_image()` never executes on the server path. We render what the template
//! would have emitted — `<|begin_of_image|><|image|><|end_of_image|>` — and the gate
//! normalises the marker on both sides. Nothing stronger is available.
//!
//! **4. `[gMASK]<sop>` is two tokens, `<arg_key>`/`<arg_value>` are single tokens.**
//! Everything listed in [`GLM_TOKENS`] is one vocab entry, so all of it must be
//! `RenderSpan::Control` — emitting `<arg_key>` as `Text` would tokenize it as six
//! ordinary tokens and diverge from training. `ControlRole` has no variants for
//! them; see `CONTRACT-GAPS` below.
//!
//! Half of them are `USER_DEFINED`, not `CONTROL`, in the GGUF token table
//! (`<think>`, `</think>`, `<tool_call>`, `</tool_call>`, `<tool_response>`,
//! `</tool_response>`, `<arg_key>`, `</arg_key>`, `<arg_value>`, `</arg_value>` are
//! ids 154841–154850, attribute `USER_DEFINED`; the `<|…|>` family and `[gMASK]`,
//! `<sop>` are `CONTROL`). So `llama_vocab_is_control` alone is **not** the test for
//! "may this literal be resolved as a control token" — it would reject exactly half
//! of this dialect. Accept `CONTROL` or `USER_DEFINED`, reject `NORMAL`.
//! `tests/fidelity/extract_template.py --tokens <gguf>` prints the table.
//!
//! **5. The generation prompt is not a transcript item, and the trait has no place
//! for it.** `<|assistant|><think>` has to be appended before the model speaks and
//! must *not* be part of `render`, because a `render` that ended with it would break
//! `render_incremental ≡ render` for every conversation whose next item is a user
//! message — the appended bytes would have to be un-appended first. It is
//! [`generation_prompt`], an inherent method, and it is exactly the head of the spans
//! the next assistant item produces (pinned by a test), so the turn engine can send
//! it, sample, and then append the turn's own spans minus those two.
//!
//! # CONTRACT-GAPS
//!
//! Three places where the fixed `letibot-dialect` contract does not fit GLM. None
//! were edited; each is worked around here and reported.
//!
//! - **GAP-1: `render_incremental(prev_end, new_items)` cannot be a pure function.**
//!   GLM's renderer needs the boundary state — is an assistant turn open, was
//!   `<think>` already emitted by the generation prompt, was the previous item a tool
//!   result — and none of it is derivable from `prev_end` alone. Worked around with
//!   [`GlmDialect::for_conversation`], which hands the instance the history the
//!   signature omits. [`GlmDialect::new`] renders incrementally only from a clean
//!   boundary and says so loudly (it panics rather than guessing). The stable prefix
//!   is never re-emitted by `render_incremental`: it is not an item, so
//!   `render(prefix, &[])` owns it and `render_incremental(0, …)` starts at item 0.
//! - **GAP-2: `ControlRole` is a closed enum with no variants for GLM's argument and
//!   media tokens.** `<arg_key>`, `</arg_key>`, `<arg_value>`, `</arg_value>`,
//!   `<sop>`, `<|begin_of_image|>`, `<|image|>`, `<|end_of_image|>` have no honest
//!   role. They are declared with [`ControlRole::TurnEnd`], the one role GLM does not
//!   use, as an explicit "no role in this contract" bucket — so
//!   `control_tokens().get(TurnEnd)` is meaningless for GLM and callers should use
//!   [`GLM_TOKENS`] by name.
//! - **GAP-3: `parse(&[u32])` cannot recover text without a vocab.** Token ids in,
//!   `ParsedSpan::Content(String)` out, from a crate with no vocab, is not
//!   implementable. [`GlmDialect::with_decoder`] takes a caller-supplied
//!   [`TokenDecoder`] (the token core owns the real one); `parse` without a decoder
//!   panics with that instruction rather than returning a plausible-looking nothing.
//!
//! See `docs/implementation-plan.md` §7 and `tests/fidelity/README.md`.

use letibot_dialect::{
    ControlToken, ControlTokens, Dialect, Guard, ParsedSpan, RenderSpan, StablePrefix,
    SystemUpdateMode,
};
use letibot_transcript::TranscriptItem;
use std::sync::{Arc, OnceLock};

mod json;
mod parse;
mod render;
mod sha256;

pub use json::{arg_value_text, glm_tool_json, hf_tojson};
pub use parse::{TableDecoder, TokenDecoder};
pub use render::{Anomaly, generation_prompt, outcome_envelope};

/// The shipped jinja this dialect was validated against, verbatim.
///
/// Extracted with:
/// `python3 tests/fidelity/extract_template.py <model-00001-of-00006.gguf>`
pub const TEMPLATE: &str = include_str!("../template/glm-5.3-flash.jinja");

/// The literal spelling of every GLM token this renderer emits.
///
/// Each of these is a **single vocab entry** (verified against the GGUF token table:
/// ids 154820–154855, `CONTROL` or `USER_DEFINED`). That is why they are emitted as
/// `RenderSpan::Control` and never as text.
pub mod tokens {
    use letibot_dialect::{ControlRole, ControlToken};

    /// The role used for GLM tokens that `ControlRole` has no variant for. See
    /// CONTRACT-GAP-2. GLM does not use `TurnEnd` for anything real — its turns end
    /// when the next turn-start token appears.
    const UNROLED: ControlRole = ControlRole::TurnEnd;

    const fn t(role: ControlRole, literal: &'static str) -> ControlToken {
        ControlToken { role, literal }
    }

    pub const GMASK: ControlToken = t(ControlRole::BeginOfText, "[gMASK]");
    pub const SOP: ControlToken = t(UNROLED, "<sop>");
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
    pub const ARG_KEY_OPEN: ControlToken = t(UNROLED, "<arg_key>");
    pub const ARG_KEY_CLOSE: ControlToken = t(UNROLED, "</arg_key>");
    pub const ARG_VALUE_OPEN: ControlToken = t(UNROLED, "<arg_value>");
    pub const ARG_VALUE_CLOSE: ControlToken = t(UNROLED, "</arg_value>");
    pub const BEGIN_OF_IMAGE: ControlToken = t(UNROLED, "<|begin_of_image|>");
    pub const IMAGE: ControlToken = t(UNROLED, "<|image|>");
    pub const END_OF_IMAGE: ControlToken = t(UNROLED, "<|end_of_image|>");
    pub const ENDOFTEXT: ControlToken = t(ControlRole::EndOfTurn, "<|endoftext|>");
}

/// Every control token the renderer can emit, for the token core to resolve once.
///
/// Order matters: the canonically-roled token for a role comes first, because
/// `ControlTokens::get` returns the first match.
pub const GLM_TOKENS: &[ControlToken] = &[
    tokens::GMASK,
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
    tokens::ENDOFTEXT,
    // CONTRACT-GAP-2: no honest role exists for these.
    tokens::SOP,
    tokens::ARG_KEY_OPEN,
    tokens::ARG_KEY_CLOSE,
    tokens::ARG_VALUE_OPEN,
    tokens::ARG_VALUE_CLOSE,
    tokens::BEGIN_OF_IMAGE,
    tokens::IMAGE,
    tokens::END_OF_IMAGE,
];

/// GLM stops a turn by starting the next one. `<|user|>` and `<|observation|>` are
/// the two ways a generation legitimately ends (a reply, or a tool call awaiting a
/// result); `<|endoftext|>` is the vocab eos.
///
/// From the GGUF: `eos=154820 <|endoftext|>`, `eot=154827 <|user|>`,
/// `eom=154829 <|observation|>`.
pub const GLM_STOP_TOKENS: &[&str] = &["<|endoftext|>", "<|user|>", "<|observation|>"];

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

/// Deliberate deviations from the training-time render, off by default.
///
/// A quirk is not a knob to tune. It exists so a divergence can be *demonstrated*
/// rather than argued about: the fidelity gate renders the whole corpus twice and
/// requires the quirked profile to match the server byte-for-byte everywhere, which
/// is what earns the right to declare the faithful profile's one difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlmQuirks {
    /// Reproduce llama.cpp's cross-iteration `{% set %}` leak: replay the most recent
    /// non-empty reasoning into any later assistant turn that has none of its own.
    /// See finding 1 in the crate docs.
    pub reasoning_leak: bool,
}

/// The conversation an incremental render is a continuation of. See CONTRACT-GAP-1.
#[derive(Debug, Clone, PartialEq)]
struct Conversation {
    prefix: StablePrefix,
    items: Vec<TranscriptItem>,
}

pub struct GlmDialect {
    quirks: GlmQuirks,
    effort: ReasoningEffort,
    conversation: Option<Arc<Conversation>>,
    decoder: Option<Arc<dyn TokenDecoder + Send + Sync>>,
}

impl std::fmt::Debug for GlmDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlmDialect")
            .field("quirks", &self.quirks)
            .field("effort", &self.effort)
            .field("conversation", &self.conversation.is_some())
            .field("decoder", &self.decoder.is_some())
            .finish()
    }
}

impl Default for GlmDialect {
    fn default() -> Self {
        Self::new()
    }
}

impl GlmDialect {
    /// A render-only dialect at default effort with no quirks.
    ///
    /// `render` and `parse` (given a decoder) are complete. `render_incremental`
    /// works only from a clean boundary — `prev_end == 0` — and panics otherwise;
    /// use [`GlmDialect::for_conversation`] for the append path. See CONTRACT-GAP-1.
    pub fn new() -> Self {
        GlmDialect {
            quirks: GlmQuirks::default(),
            effort: ReasoningEffort::default(),
            conversation: None,
            decoder: None,
        }
    }

    /// A dialect bound to the conversation it is appending to.
    ///
    /// `items` is the whole transcript rendered so far. `render_incremental(k, new)`
    /// then replays `items[..k]` to recover the boundary state that the trait's
    /// signature does not carry. It never re-emits the stable prefix: the prefix is
    /// not an item, so `render(prefix, &[])` owns it and `render_incremental(0, …)`
    /// starts at item 0.
    pub fn for_conversation(mut self, prefix: StablePrefix, items: Vec<TranscriptItem>) -> Self {
        self.conversation = Some(Arc::new(Conversation { prefix, items }));
        self
    }

    pub fn with_quirks(mut self, quirks: GlmQuirks) -> Self {
        self.quirks = quirks;
        self
    }

    pub fn with_effort(mut self, effort: ReasoningEffort) -> Self {
        self.effort = effort;
        self
    }

    /// Supply the id→text map `parse` needs. See CONTRACT-GAP-3.
    pub fn with_decoder(mut self, decoder: Arc<dyn TokenDecoder + Send + Sync>) -> Self {
        self.decoder = Some(decoder);
        self
    }

    pub fn quirks(&self) -> GlmQuirks {
        self.quirks
    }

    pub fn effort(&self) -> ReasoningEffort {
        self.effort
    }

    /// Report transcript shapes this renderer cannot render faithfully.
    ///
    /// Declarative on purpose, in the same spirit as `Guard`: the dialect says what
    /// is wrong, the caller decides. Empty means the render will match the shipped
    /// template (modulo the declared divergences in the crate docs).
    pub fn check_transcript(&self, items: &[TranscriptItem]) -> Vec<Anomaly> {
        render::check_transcript(items)
    }

    /// True when the transcript ends with an assistant turn already open, i.e. the
    /// render carries no generation prompt.
    ///
    /// The fidelity runner needs it to set `add_generation_prompt` on the oracle
    /// request, and the turn engine needs it to know whether it is starting a turn or
    /// continuing one.
    pub fn ends_mid_turn(&self, items: &[TranscriptItem]) -> bool {
        render::ends_mid_turn(items)
    }
}

impl Dialect for GlmDialect {
    fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan> {
        let mut out = Vec::new();
        render::render_prefix(self.effort, prefix, &mut out);
        let mut st = render::State::default();
        render::render_items(self.quirks, items, &mut st, &mut out);
        out
    }

    fn render_incremental(&self, prev_end: usize, new_items: &[TranscriptItem]) -> Vec<RenderSpan> {
        let empty = Vec::new();
        let (prefix, history) = match &self.conversation {
            Some(c) => (Some(&c.prefix), &c.items),
            None => (None, &empty),
        };

        if prev_end > history.len() {
            panic!(
                "GlmDialect::render_incremental was asked to continue from item {prev_end} but was \
                 given no history (CONTRACT-GAP-1: the trait's (prev_end, new_items) signature does \
                 not carry the boundary state GLM's renderer needs). Build the dialect with \
                 GlmDialect::for_conversation(prefix, items) before appending."
            );
        }

        // The stable prefix is not an item, so it is never re-emitted here: at
        // prev_end == 0 the caller has already rendered it with `render(prefix, &[])`.
        let _ = prefix;

        let mut out = Vec::new();
        let mut st = render::State::default();
        if prev_end > 0 {
            // Replay the already-rendered items for their effect on the state only.
            let mut discard = Vec::new();
            render::render_items(self.quirks, &history[..prev_end], &mut st, &mut discard);
        }
        render::render_items(self.quirks, new_items, &mut st, &mut out);
        out
    }

    fn parse(&self, tokens: &[u32]) -> Vec<ParsedSpan> {
        let decoder = self.decoder.as_deref().unwrap_or_else(|| {
            panic!(
                "GlmDialect::parse needs an id->text decoder (CONTRACT-GAP-3: the trait's \
                 parse(&[u32]) cannot recover Content(String) from a crate with no vocab). \
                 Build the dialect with GlmDialect::with_decoder(...)."
            )
        });
        parse::parse(decoder, tokens)
    }

    fn control_tokens(&self) -> ControlTokens {
        ControlTokens(GLM_TOKENS)
    }

    fn stop_tokens(&self) -> &'static [&'static str] {
        GLM_STOP_TOKENS
    }

    /// GLM reads a system message wherever it sits — verified: a `system` message at
    /// index 2 renders as `<|system|>…` in place, between the surrounding turns. So a
    /// mid-session prompt change is appended, and costs nothing beyond its own tokens.
    fn system_update_mode(&self) -> SystemUpdateMode {
        SystemUpdateMode::InHistory
    }

    fn guards(&self) -> &'static [Guard] {
        // §7.3: the repeating-token collapse past ~78k at -ub 512 poisons the slot
        // rather than erroring, and does not reproduce on this build (probed to
        // 147,042 tokens). It costs nothing to watch for, and 1M has never been run.
        // ReasoningStall is not a cap on thinking — long thinking is fine — it is a
        // signal that the turn stopped progressing.
        &[
            Guard::RepetitionRun { run: 64 },
            Guard::RepetitionNgram {
                window: 32,
                times: 8,
            },
            Guard::ReasoningStall { tokens: 65536 },
        ]
    }

    fn template_sha(&self) -> [u8; 32] {
        static SHA: OnceLock<[u8; 32]> = OnceLock::new();
        *SHA.get_or_init(|| sha256::sha256(TEMPLATE.as_bytes()))
    }
}

/// Hex of a `template_sha`, for logging and for the staleness check.
pub fn sha_hex(sha: &[u8; 32]) -> String {
    sha256::hex(sha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_dialect::ControlRole;

    #[test]
    fn template_sha_is_the_hash_of_the_shipped_jinja() {
        // sha256sum of tokenizer.chat_template extracted from
        // GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf, 2026-09-09.
        assert_eq!(
            sha_hex(&GlmDialect::new().template_sha()),
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
            assert!(seen.insert(t.literal), "duplicate literal {}", t.literal);
        }
    }

    #[test]
    fn canonical_roles_resolve_to_the_canonical_token() {
        // GAP-2 puts several literals under TurnEnd. That must not shadow a real one.
        let ct = ControlTokens(GLM_TOKENS);
        assert_eq!(ct.get(ControlRole::TurnStartUser).unwrap().literal, "<|user|>");
        assert_eq!(ct.get(ControlRole::ThinkOpen).unwrap().literal, "<think>");
        assert_eq!(
            ct.get(ControlRole::ToolCallOpen).unwrap().literal,
            "<tool_call>"
        );
        assert_eq!(ct.get(ControlRole::BeginOfText).unwrap().literal, "[gMASK]");
    }
}
