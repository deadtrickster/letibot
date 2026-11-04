//! What a per-model dialect is: rendering is data, parsing is code.
//! `RenderSpan` is text plus control token, so a renderer needs no tokenizer or GPU, and a user
//! message containing `<|assistant|>` cannot become the control token.

use std::borrow::Cow;
use std::ops::Range;

/// The cached, unchanging head of a prompt: bootstrap system prompt and tool schemas.
/// Separated from the items because it is what the server's prefix cache is keyed on - a
/// change here invalidates every downstream token, a change in `items` does not.
#[derive(Debug, Clone, PartialEq)]
pub struct StablePrefix {
    pub system: String,
    /// Tool schemas as JSON text, in a fixed order. Order is part of the prefix:
    /// reordering these re-prefills the whole conversation.
    pub tools_json: Vec<String>,
}

/// One piece of a rendered prompt, before tokenization. Still what `tokencore` consumes;
/// only who produces it changed, a template-driven renderer splitting the template's output
/// on control-token literals.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderSpan {
    /// Literal text. Tokenized with special-token parsing disabled, so nothing
    /// inside it can become a control token however it is spelled.
    Text(String),
    /// A control token, resolved to an exact id by the token core.
    Control(ControlToken),
}

/// A control token, identified symbolically and literally. The `literal` is what the shipped
/// jinja emits and what the fidelity gate checks; the `role` is what `Parser` and the
/// invariant tests reason about. `Cow` because a GGUF-loaded dialect cannot be `&'static`.
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

    /// A control token whose literal was discovered at runtime - read out of a GGUF's
    /// token table, or out of a config file.
    pub fn owned(role: ControlRole, literal: impl Into<String>) -> Self {
        ControlToken {
            role,
            literal: Cow::Owned(literal.into()),
        }
    }
}

/// The structural boundaries a parser can recognise. Every variant is a boundary some model
/// spells as a single vocab entry, so every one must be emitted as `RenderSpan::Control`:
/// spelling `<arg_key>` as text tokenizes it as six ordinary tokens and diverges from training.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControlRole {
    BeginOfText,
    /// GLM's `<sop>`: the start-of-prompt token that follows `[gMASK]`. One vocab
    /// entry, no turn semantics, and it is not a `BeginOfText` - both are emitted.
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
    /// `<arg_key>` - GLM writes tool-call arguments as a flat sequence of
    /// key/value pairs rather than as JSON, and each delimiter is one vocab entry.
    ArgKeyOpen,
    ArgKeyClose,
    ArgValueOpen,
    ArgValueClose,
    ImageOpen,
    Image,
    ImageClose,
    EndOfTurn,
    /// A single-entry token with no structural role this contract models. Named rather than
    /// smuggled in under an unrelated variant: `Other` is greppable, and `all_with_role(Other)`
    /// lists exactly the tokens nobody has modelled yet.
    Other,
}

/// Every control token a dialect uses, so the token core can resolve them once at startup and
/// fail loudly if the vocab does not contain one. A slice rather than named fields, because
/// models disagree about which boundaries exist and an `Option` per role invites a quiet skip.
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

    /// The primary lookup. A literal is what the template emits and what the vocabulary resolves
    /// to exactly one id. Two entries may share a literal under different roles, which is legal:
    /// they resolve to the same id and only the label differs.
    pub fn by_literal(&self, literal: &str) -> Option<&ControlToken> {
        self.0.iter().find(|t| t.literal == literal)
    }

    /// Every token carrying a role. Returning an iterator rather than one entry makes "this role
    /// has three literals" a thing the caller must look at, instead of a convention held up by
    /// the table's order.
    pub fn all_with_role(&self, role: ControlRole) -> impl Iterator<Item = &ControlToken> {
        self.0.iter().filter(move |t| t.role == role)
    }
}

/// A literal that ends generation, with the role it plays. The role is not decoration: a stop
/// token that fails to resolve never fires and the turn runs to `n_ctx` with nothing in the
/// logs saying why, so the startup failure can name what was lost.
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

/// What `parse` recovers from a decoded token stream. `parse ∘ render` must be the identity on
/// the round-trippable parts, which catches a control-token mistake before a model does, and
/// every span carries the id range it came from so the ledger need not re-segment the stream.
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

/// The vocabulary, as much of it as a parser is allowed to know: the smallest interface that
/// lets a parser return `Content(String)` from ids in a crate with no vocab. `decode` takes a
/// slice, not one id, because detokenization is not per-token concatenation on a BPE vocabulary.
pub trait TokenDecoder {
    /// Text for a run of ids. Infallible on purpose: a parser that could fail here would have to
    /// choose between dropping bytes and propagating an error, and dropping bytes is the failure
    /// this design exists to abolish. An implementation that cannot decode must render a marker.
    fn decode(&self, tokens: &[u32]) -> String;

    /// The role of a single id, if it is one of this dialect's control tokens. By id, not by
    /// matching decoded text: text that merely spells `</think>` is several ids and can never be
    /// mistaken for the boundary.
    fn control_role(&self, token: u32) -> Option<ControlRole>;
}

/// How a mid-session system-prompt change is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemUpdateMode {
    /// The model reads the latest `system` message wherever it sits, so a change is appended
    /// after the cached history: adding one line to an `<env>` block by rewriting message 0
    /// costs a full cold re-prefill of the conversation.
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
    /// Not a cap on thinking - a signal that the turn is not progressing. Long
    /// thinking is fine; silent non-termination is not.
    ReasoningStall { tokens: u32 },
}

/// Which wire field a model replays its own reasoning into. A per-model fact, so `DialectSpec`
/// carries it and the engine reads it from there rather than from configuration.
/// `letibot-transcript` has a mirrored enum this crate declines to depend on: keep them in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningField {
    /// GLM: replayed as `reasoning_content`.
    ReasoningContent,
    /// Qwen: replayed inside the rendered turn, gated by `preserve_thinking`.
    Inline,
}

/// Everything about a model that rendering needs, as a value. There is no method here because
/// there is nothing left to implement: the renderer is one template engine shared by every
/// model, and what distinguishes GLM from Qwen is the `template` string plus the tables beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct DialectSpec {
    pub name: Cow<'static, str>,
    /// The jinja source, as shipped in the GGUF's `tokenizer.chat_template`.
    pub template: Cow<'static, str>,
    /// SHA-256 of `template`. A model update that changes the template invalidates the
    /// spec automatically, and this is the dialect's identity in the store.
    pub template_sha: [u8; 32],
    pub control_tokens: ControlTokens,
    pub stop_tokens: Vec<StopToken>,
    pub system_update_mode: SystemUpdateMode,
    /// Which wire field this model replays its own reasoning into.
    pub reasoning_field: ReasoningField,
    pub guards: Vec<Guard>,
}

/// Token ids in, spans out. Parsing is not data because it is not a template: the shipped jinja
/// says how a turn is written, never how to read one back, and every model's `<tool_call>`
/// grammar has to be walked by something that knows its shape.
pub trait Parser: Send + Sync {
    /// Takes `u32` rather than a `llama_token` alias so this crate stays free of the FFI, and
    /// takes the `decoder` because it must. `reasoning_open` is the channel the model is speaking
    /// on before the first token of the slice, which the parser cannot recover from ids alone.
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

/// The same spans as the STRING a server tokenizes itself, for a prompt carrying media:
/// llama.cpp takes images only as a `prompt_string`, substituting the picture at the media
/// marker, and one image is one marker. Safe for the cache because control tokens are atomic.
pub fn spans_to_string_with_media(spans: &[RenderSpan], marker: &str) -> String {
    let mut s = String::new();
    for span in spans {
        match span {
            RenderSpan::Text(t) => s.push_str(t),
            RenderSpan::Control(c) => match c.role {
                ControlRole::ImageOpen => s.push_str(marker),
                ControlRole::Image | ControlRole::ImageClose => {}
                _ => s.push_str(&c.literal),
            },
        }
    }
    s
}

/// How many images a span sequence places, one per `ImageOpen`. The engine pairs this with
/// `media_in_order`: the server substitutes pictures in marker order, so the two lists must be
/// the same length and order. A test in `letibot-turn` holds the renderer to it.
pub fn media_spans(spans: &[RenderSpan]) -> usize {
    spans
        .iter()
        .filter(|s| matches!(s, RenderSpan::Control(c) if c.role == ControlRole::ImageOpen))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKENS: &[ControlToken] = &[
        ControlToken::borrowed(ControlRole::TurnStartUser, "<|user|>"),
        ControlToken::borrowed(ControlRole::TurnStartAssistant, "<|assistant|>"),
        // One role, three literals: the case resolved by "whichever
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

    /// The reason `Cow` is not cosmetic: this is a dialect nobody compiled, and every string is
    /// owned as it would come out of a GGUF's `tokenizer.chat_template` at runtime. `&'static str`
    /// cannot be produced from a `String` read at startup without leaking it.
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

    /// An owned table answers exactly as a borrowed one does: the same value, not a second
    /// implementation, so a dialect read out of a GGUF cannot answer a lookup differently from a
    /// compiled one just because its literals were allocated.
    #[test]
    fn an_owned_table_answers_exactly_as_a_borrowed_one_does() {
        let owned = ControlTokens::owned(
            TOKENS
                .iter()
                .map(|t| ControlToken::owned(t.role, t.literal.to_string()))
                .collect(),
        );
        let borrowed = ControlTokens::borrowed(TOKENS);
        assert_eq!(owned, borrowed);

        for t in TOKENS {
            assert_eq!(
                owned.by_literal(&t.literal),
                borrowed.by_literal(&t.literal)
            );
            let owned_role: Vec<&str> = owned
                .all_with_role(t.role)
                .map(|x| x.literal.as_ref())
                .collect();
            let borrowed_role: Vec<&str> = borrowed
                .all_with_role(t.role)
                .map(|x| x.literal.as_ref())
                .collect();
            assert_eq!(owned_role, borrowed_role);
        }

        assert_eq!(
            StopToken::owned(ControlRole::TurnEnd, "<|im_end|>"),
            StopToken::borrowed(ControlRole::TurnEnd, "<|im_end|>")
        );
    }
}
