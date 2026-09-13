//! What a per-model dialect is: **rendering is data, parsing is code.**
//!
//! # Why this crate has no vocab, no FFI and no model
//!
//! `RenderSpan` is deliberately **text-plus-control-token, not token ids**. That one
//! choice is what keeps dialect work off the critical path: a renderer that emits
//! `[Text | Control]` is a pure function, needs no tokenizer, no vocab, no daemon and
//! no GPU, and can be acceptance-tested from Python against the shipped template.
//! Had rendering returned `Vec<llama_token>`, every dialect would block on the token
//! core's FFI for no reason at all.
//!
//! There is a second, larger reason, and it is a correctness one.
//!
//! **A user message containing the literal text `<|assistant|>` must never become the
//! assistant control token.** If a renderer produced one flat string that was later
//! tokenized with special-token parsing enabled, it would — and the model would see a
//! turn boundary the harness never wrote. Splitting `Text` from `Control` makes that
//! *structurally impossible* rather than something a test has to catch: `Text` is
//! tokenized with special-token parsing OFF, and control tokens are resolved to exact
//! ids by the token core. Neither path can produce the other's output.
//!
//! # Why there is no `render` here (T1/D11)
//!
//! T1 settled that we run the **model's own shipped jinja** through a Rust Jinja
//! engine rather than hand-writing a renderer per model
//! (`experiments/minijinja-fidelity/RESULTS.md`). Three things followed immediately,
//! and they are why this file looks the way it does:
//!
//! * **Rendering became data.** Everything a renderer needs about a model — the
//!   template source, the control-token set, the stop tokens, the system-update rule,
//!   the guards — is a value now, [`DialectSpec`], not a set of methods. A dialect
//!   read out of a GGUF at runtime is then a *value someone constructs*, not a type
//!   someone writes.
//! * **There is no incremental render to specify.** Jinja has no incremental mode, so
//!   there is no boundary state for a `render_incremental` signature to carry, and no
//!   place for the generation prompt to hide: it is a template argument
//!   (`add_generation_prompt`).
//! * **Parsing stayed code**, and every unresolved defect in the old contract was on
//!   that side. Hence [`Parser`], which is small, and [`TokenDecoder`], which is the
//!   thing that makes it implementable at all.
//!
//! Because a runtime-loaded template cannot produce a `&'static str`, every literal
//! this crate traffics in is [`Cow<'static, str>`]. Compile-time dialects use
//! `Cow::Borrowed` and pay nothing; a GGUF-loaded one uses `Cow::Owned` and is
//! expressible at all.
//!
//! See `docs/implementation-plan.md` §7 and `docs/workstreams.md` §3.

use std::borrow::Cow;
use std::ops::Range;

/// The cached, unchanging head of a prompt: bootstrap system prompt and tool schemas.
///
/// Separated from the items because it is what the server's prefix cache is keyed on.
/// A change here invalidates every downstream token; a change in `items` does not.
#[derive(Debug, Clone, PartialEq)]
pub struct StablePrefix {
    pub system: String,
    /// Tool schemas as JSON text, in a fixed order. Order is part of the prefix:
    /// reordering these re-prefills the whole conversation.
    pub tools_json: Vec<String>,
}

/// One piece of a rendered prompt, before tokenization.
///
/// Still the renderer's output type and still what `tokencore` consumes. What changed
/// under T1 is only *who* produces it: a template-driven renderer splits the template's
/// output on control-token literals instead of emitting the spans directly.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderSpan {
    /// Literal text. **Tokenized with special-token parsing disabled**, so nothing
    /// inside it can become a control token however it is spelled.
    Text(String),
    /// A control token, resolved to an exact id by the token core.
    Control(ControlToken),
}

/// A control token, identified symbolically *and* literally.
///
/// The `literal` is what the shipped jinja emits and is what the fidelity gate checks
/// against. The `role` is what [`Parser`] and the invariant tests reason about, so a
/// dialect that spells a boundary differently is still comparable to one that does not.
///
/// `literal` is a [`Cow`] because a dialect loaded from a GGUF at runtime cannot
/// produce a `&'static str`. Compile-time dialects build these in `const` context with
/// [`ControlToken::borrowed`] and allocate nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ControlToken {
    pub role: ControlRole,
    pub literal: Cow<'static, str>,
}

impl ControlToken {
    /// A compile-time control token. `const`, so a dialect's whole token table stays a
    /// `const` slice.
    pub const fn borrowed(role: ControlRole, literal: &'static str) -> Self {
        ControlToken {
            role,
            literal: Cow::Borrowed(literal),
        }
    }

    /// A control token whose literal was discovered at runtime — read out of a GGUF's
    /// token table, or out of a config file.
    pub fn owned(role: ControlRole, literal: impl Into<String>) -> Self {
        ControlToken {
            role,
            literal: Cow::Owned(literal.into()),
        }
    }
}

/// The structural boundaries a parser can recognise.
///
/// Every variant here is a boundary some model spells as a **single vocab entry**, so
/// every one of them has to be emitted as [`RenderSpan::Control`] rather than as text:
/// spelling `<arg_key>` as text tokenizes it as six ordinary tokens and diverges from
/// training.
///
/// `Ord` is derived so a diagnostic listing several roles — a failed control-token
/// resolution, say — can sort rather than depend on declaration order in some table.
/// The order itself carries no meaning beyond determinism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControlRole {
    BeginOfText,
    /// GLM's `<sop>`: the start-of-prompt token that follows `[gMASK]`. One vocab
    /// entry, no turn semantics, and it is not a `BeginOfText` — both are emitted.
    SequenceStart,
    TurnStartSystem,
    TurnStartUser,
    TurnStartAssistant,
    TurnStartTool,
    TurnEnd,
    ThinkOpen,
    ThinkClose,
    ToolCallOpen,
    ToolCallClose,
    ToolResultOpen,
    ToolResultClose,
    /// `<arg_key>` — GLM writes tool-call arguments as a flat sequence of
    /// key/value pairs rather than as JSON, and each delimiter is one vocab entry.
    ArgKeyOpen,
    ArgKeyClose,
    ArgValueOpen,
    ArgValueClose,
    ImageOpen,
    Image,
    ImageClose,
    EndOfTurn,
    /// A single-entry token with no structural role this contract models.
    ///
    /// Named rather than smuggled in under some unrelated variant. The predecessor of
    /// this enum had no `Other`, so `dialect-glm` declared eight tokens as `TurnEnd`
    /// — a role it never otherwise used — and the only way to find out was to read a
    /// comment. `Other` is greppable, and `all_with_role(Other)` lists exactly the
    /// tokens nobody has modelled yet.
    Other,
}

/// Every control token a dialect uses, so the token core can resolve them once at
/// startup and fail loudly if the vocab does not contain one.
///
/// A slice rather than a struct of named fields: models disagree about which
/// boundaries exist at all, and an `Option` per role invites a renderer to quietly
/// skip one it should have emitted. A [`Cow`] slice rather than `&'static`, for the
/// same reason [`ControlToken::literal`] is one.
///
/// **One role may own many literals** and that is the case that matters — see
/// [`ControlTokens::all_with_role`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlTokens(pub Cow<'static, [ControlToken]>);

impl ControlTokens {
    /// A compile-time token table.
    pub const fn borrowed(tokens: &'static [ControlToken]) -> Self {
        ControlTokens(Cow::Borrowed(tokens))
    }

    pub fn owned(tokens: Vec<ControlToken>) -> Self {
        ControlTokens(Cow::Owned(tokens))
    }

    pub fn as_slice(&self) -> &[ControlToken] {
        &self.0
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ControlToken> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// **The primary lookup.** A literal is what the template emits and what the
    /// vocabulary resolves to exactly one id, so this is the direction that is always
    /// unambiguous in the way that matters.
    ///
    /// Two entries may share a literal under different roles — a model that ends a
    /// turn and ends a message with the same token — and that is legal. They resolve
    /// to the same id; only the label differs, and this returns the first.
    pub fn by_literal(&self, literal: &str) -> Option<&ControlToken> {
        self.0.iter().find(|t| t.literal == literal)
    }

    /// Every token carrying a role.
    ///
    /// This replaces a `get(role) -> Option<ControlToken>` that returned *whichever
    /// entry came first*. `dialect-glm` kept its table in an order chosen so that the
    /// canonical entry won — a convention held up by a comment, which is exactly the
    /// kind of guarantee that stops being true when somebody sorts the table.
    /// Returning an iterator makes "this role has three literals" a thing the caller
    /// must look at rather than a thing it silently gets one of.
    pub fn all_with_role(&self, role: ControlRole) -> impl Iterator<Item = &ControlToken> {
        self.0.iter().filter(move |t| t.role == role)
    }
}

/// A literal that ends generation, with the role it plays.
///
/// The role is not decoration. A stop token that fails to resolve — absent from the
/// vocabulary, or silently a multi-token *sequence* rather than one entry — never
/// fires, and the turn runs to `n_ctx` with nothing in the logs saying why. Carrying
/// the role means the startup failure can name what was lost ("the tool-call boundary
/// `<|observation|>` is not one token") instead of printing a bare string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StopToken {
    pub literal: Cow<'static, str>,
    pub role: ControlRole,
}

impl StopToken {
    pub const fn borrowed(role: ControlRole, literal: &'static str) -> Self {
        StopToken {
            role,
            literal: Cow::Borrowed(literal),
        }
    }

    pub fn owned(role: ControlRole, literal: impl Into<String>) -> Self {
        StopToken {
            role,
            literal: Cow::Owned(literal.into()),
        }
    }
}

/// What `parse` recovers from a decoded token stream.
///
/// `parse ∘ render` must be the identity on the round-trippable parts: render an
/// assistant turn with reasoning and two tool calls, parse it back, get the same
/// structure. That property catches a control-token mistake before a model does.
///
/// Every span carries the `Range<usize>` of the token ids it was parsed from —
/// indices into the slice handed to [`Parser::parse`]. The ledger needs to know
/// which tokens belong to an item, and only the parser walks the ids; without the
/// offsets, the caller had to re-segment the stream on control roles and parse
/// segment by segment, which is a second parser in every consumer.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedSpan {
    Reasoning {
        text: String,
        range: Range<usize>,
    },
    Content {
        text: String,
        range: Range<usize>,
    },
    ToolCall {
        id: Option<String>,
        name: String,
        arguments: String,
        range: Range<usize>,
    },
    Control {
        role: ControlRole,
        range: Range<usize>,
    },
}

/// The vocabulary, as much of it as a parser is allowed to know.
///
/// A parser is handed token **ids** and must return `Content(String)`, which is not
/// implementable in a crate with no vocab — that was the oldest unfixed hole in the
/// old contract. This is the smallest interface that closes it, and the token core is
/// the only thing that implements it for real.
///
/// Note `decode` takes a **slice**, not one id. Detokenization is not per-token
/// concatenation on a BPE vocabulary, so decoding a run and joining the pieces are
/// different operations, and only the first one is correct.
pub trait TokenDecoder {
    /// Text for a run of ids. Infallible on purpose: a parser that could fail here
    /// would have to choose between dropping bytes and propagating an error through
    /// every caller, and dropping bytes is the failure this whole design exists to
    /// abolish. An implementation that cannot decode something must render a visible
    /// marker instead.
    fn decode(&self, tokens: &[u32]) -> String;

    /// The role of a single id, if it is one of this dialect's control tokens.
    ///
    /// By id, not by matching decoded text: a control token is one vocab entry, so
    /// text that merely *spells* `</think>` is several ids and can never be mistaken
    /// for the boundary. That is the same guarantee [`RenderSpan`] gives on the way
    /// out, restated on the way back in.
    fn control_role(&self, token: u32) -> Option<ControlRole>;
}

/// How a mid-session system-prompt change is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemUpdateMode {
    /// The model reads the latest `system` message wherever it sits, so a change is
    /// **appended** after the cached history. DeepSeek's choice, and the reason it
    /// matters: adding one line to an `<env>` block by rewriting message 0 cost a
    /// full cold re-prefill of a 179k-token conversation when measured.
    InHistory,
    /// The model only honours a system prompt at position 0, so a change forces a
    /// re-render. A dialect declaring this is declaring a known, priced cost.
    Envelope,
}

/// A declared, model-specific mitigation. Declarative on purpose: the dialect says
/// *what* to watch for, the turn engine owns *what to do*, so a dialect stays data and
/// guards stay testable without a running model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Stop when the same token repeats `run` times consecutively.
    RepetitionRun { run: u32 },
    /// Stop when an n-gram of `window` tokens repeats `times` times.
    RepetitionNgram { window: u32, times: u32 },
    /// Reasoning ran past `tokens` without producing a content or tool-call span.
    /// Not a cap on thinking — a signal that the turn is not progressing. Long
    /// thinking is fine; silent non-termination is not.
    ReasoningStall { tokens: u32 },
}

/// Which wire field a model replays its own reasoning into.
///
/// A per-model fact, which is why [`DialectSpec`] carries it and the engine reads
/// it from there rather than from configuration. `letibot-transcript` has a
/// mirrored enum it stamps on `TranscriptItem::Reasoning` rows as provenance —
/// this crate cannot see that one without taking a dependency, which its
/// Cargo.toml declines, so `crates/turn` maps between them at its single
/// `produce` call. Keep the variants in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningField {
    /// GLM: replayed as `reasoning_content`.
    ReasoningContent,
    /// Qwen: replayed inside the rendered turn, gated by `preserve_thinking`.
    Inline,
}

/// Everything about a model that rendering needs — as a value.
///
/// This is the half of the old `Dialect` trait that T1 dissolved. There is no method
/// here because there is nothing left to implement: the renderer is one template
/// engine shared by every model, and what distinguishes GLM from Qwen is the
/// `template` string plus the tables beside it.
///
/// The practical consequence is that a **new model needs no code**. Extract the
/// template and the token table from its GGUF, construct one of these, and the
/// renderer works — which is the whole reason T1 was run.
#[derive(Debug, Clone, PartialEq)]
pub struct DialectSpec {
    pub name: Cow<'static, str>,
    /// The jinja source, as shipped in the GGUF's `tokenizer.chat_template`.
    pub template: Cow<'static, str>,
    /// SHA-256 of `template`. A model update that changes the template invalidates the
    /// spec automatically, and §4.4 makes this the dialect's identity in the store.
    pub template_sha: [u8; 32],
    pub control_tokens: ControlTokens,
    pub stop_tokens: Vec<StopToken>,
    pub system_update_mode: SystemUpdateMode,
    /// Which wire field this model replays its own reasoning into.
    pub reasoning_field: ReasoningField,
    pub guards: Vec<Guard>,
}

/// Token ids in, spans out. The half of the old contract that stayed code.
///
/// Parsing is not data because it is not a template: the shipped jinja says how a turn
/// is written, never how to read one back, and every model's `<tool_call>` grammar has
/// to be walked by something that knows its shape.
pub trait Parser: Send + Sync {
    /// Takes `u32` rather than a `llama_token` alias so this crate stays free of the
    /// FFI, and takes the `decoder` because it must: see [`TokenDecoder`].
    ///
    /// `reasoning_open` is the channel the model is speaking on **before the first
    /// token of the slice**: a generation prompt commonly leaves the reasoning block
    /// open, and a slice parsed from a stateless `Content` start would promote a cut
    /// -off deliberation to visible content — §5.7's `ReasoningOnly` failure
    /// masquerading as a `TruncatedText` success. The caller owns that fact because
    /// the caller owns the lead; the parser cannot recover it from ids alone.
    fn parse(
        &self,
        tokens: &[u32],
        decoder: &dyn TokenDecoder,
        reasoning_open: bool,
    ) -> Vec<ParsedSpan>;
}

/// Concatenate the text of a span sequence. The detokenized form of a render must
/// equal the shipped template's output exactly; this is the "ours" half of that
/// comparison and is what the W3 fidelity gate calls.
pub fn spans_to_string(spans: &[RenderSpan]) -> String {
    let mut s = String::new();
    for span in spans {
        match span {
            RenderSpan::Text(t) => s.push_str(t),
            RenderSpan::Control(c) => s.push_str(&c.literal),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKENS: &[ControlToken] = &[
        ControlToken::borrowed(ControlRole::TurnStartUser, "<|user|>"),
        ControlToken::borrowed(ControlRole::TurnStartAssistant, "<|assistant|>"),
        // One role, three literals: the case that used to be resolved by "whichever
        // comes first in the table".
        ControlToken::borrowed(ControlRole::TurnEnd, "<|im_end|>"),
        ControlToken::borrowed(ControlRole::TurnEnd, "<|endoftext|>"),
        ControlToken::borrowed(ControlRole::TurnEnd, "<|eot_id|>"),
    ];

    #[test]
    fn a_control_literal_inside_user_text_stays_text() {
        // The injection case. Text and Control are different variants, so no amount
        // of adversarial user content can produce a turn boundary.
        let spans = vec![
            RenderSpan::Control(TOKENS[0].clone()),
            RenderSpan::Text("please print <|assistant|> verbatim".into()),
        ];
        assert_eq!(
            spans
                .iter()
                .filter(|s| matches!(s, RenderSpan::Control(_)))
                .count(),
            1,
            "user text must not add a control span"
        );
        // It still round-trips to the right string for the fidelity diff.
        assert_eq!(
            spans_to_string(&spans),
            "<|user|>please print <|assistant|> verbatim"
        );
    }

    #[test]
    fn lookup_is_by_literal_and_a_role_may_own_several() {
        let ct = ControlTokens::borrowed(TOKENS);
        assert_eq!(
            ct.by_literal("<|assistant|>").unwrap().role,
            ControlRole::TurnStartAssistant
        );
        assert!(ct.by_literal("<think>").is_none());

        let ends: Vec<&str> = ct
            .all_with_role(ControlRole::TurnEnd)
            .map(|t| t.literal.as_ref())
            .collect();
        assert_eq!(ends, ["<|im_end|>", "<|endoftext|>", "<|eot_id|>"]);
        assert_eq!(ct.all_with_role(ControlRole::ThinkOpen).count(), 0);
    }

    #[test]
    fn roles_sort_so_a_listing_can_be_deterministic() {
        let mut roles = [
            ControlRole::Other,
            ControlRole::TurnStartUser,
            ControlRole::BeginOfText,
        ];
        roles.sort();
        assert_eq!(
            roles,
            [
                ControlRole::BeginOfText,
                ControlRole::TurnStartUser,
                ControlRole::Other
            ]
        );
    }

    /// The reason `Cow` is not cosmetic: this is a dialect nobody compiled.
    ///
    /// Every string here is owned, as it would be coming out of a GGUF's
    /// `tokenizer.chat_template` and token table at runtime. Under the old contract
    /// this function could not be written at all — `&'static str` cannot be produced
    /// from a `String` read at startup without leaking it.
    #[test]
    fn a_spec_can_be_built_entirely_at_runtime() {
        let template: String = "{% for m in messages %}{{ m.content }}{% endfor %}".to_string();
        let literals: Vec<(ControlRole, String)> = vec![
            (ControlRole::TurnStartUser, "<|user|>".to_string()),
            (ControlRole::TurnStartAssistant, "<|assistant|>".to_string()),
        ];

        let spec = DialectSpec {
            name: Cow::Owned("read-from-a-gguf".to_string()),
            template: Cow::Owned(template.clone()),
            template_sha: [0u8; 32],
            control_tokens: ControlTokens::owned(
                literals
                    .into_iter()
                    .map(|(role, lit)| ControlToken::owned(role, lit))
                    .collect(),
            ),
            stop_tokens: vec![StopToken::owned(ControlRole::EndOfTurn, "<|endoftext|>")],
            system_update_mode: SystemUpdateMode::InHistory,
            reasoning_field: ReasoningField::Inline,
            guards: vec![Guard::RepetitionRun { run: 64 }],
        };

        assert_eq!(spec.template, template);
        assert!(matches!(spec.name, Cow::Owned(_)));
        assert_eq!(
            spec.control_tokens.by_literal("<|user|>").unwrap().role,
            ControlRole::TurnStartUser
        );
        assert_eq!(spec.stop_tokens[0].role, ControlRole::EndOfTurn);
    }
}
