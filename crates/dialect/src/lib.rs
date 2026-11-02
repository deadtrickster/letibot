//! What a per-model dialect is: rendering is data, parsing is code.
//!
//! [`RenderSpan`] is text-plus-control-token rather than token ids, which keeps dialect
//! work off the critical path: a renderer that emits `[Text | Control]` is a pure
//! function needing no tokenizer, no vocab, no daemon and no GPU.
//!
//! The larger reason is correctness: a user message containing the literal text
//! `<|assistant|>` must never become the assistant control token. `Text` is tokenized
//! with special-token parsing off and a control token resolves to an exact id, so
//! neither path can produce the other's output.
//!
//! A dialect is a value ([`DialectSpec`]) because we run the model's own shipped jinja
//! instead of a renderer written per model. Jinja has no incremental mode, so there is
//! no incremental render to specify; parsing stayed code, because a tool-call grammar
//! has to be walked by something that knows its shape.
//!
//! Every literal is [`Cow<'static, str>`] because a template read out of a GGUF at
//! runtime cannot produce a `&'static str`. Compile-time dialects use `Cow::Borrowed`
//! and allocate nothing.

use std::borrow::Cow;
use std::ops::Range;

/// The cached, unchanging head of a prompt: bootstrap system prompt and tool schemas. It is what
/// the server's prefix cache is keyed on, so a change here invalidates every downstream token.
#[derive(Debug, Clone, PartialEq)]
pub struct StablePrefix {
    pub system: String,
    /// Tool schemas as JSON text, in a fixed order. Order is part of the prefix:
    /// reordering these re-prefills the whole conversation.
    pub tools_json: Vec<String>,
}

/// One piece of a rendered prompt, before tokenization: a template-driven renderer splits the
/// template's output on control-token literals to build these.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderSpan {
    /// Literal text, tokenized with special-token parsing disabled, so nothing inside
    /// it can become a control token however it is spelled.
    Text(String),
    /// A control token, resolved to an exact id by the token core.
    Control(ControlToken),
}

/// A control token, identified symbolically and literally: the literal is what the shipped jinja
/// emits, the role is what [`Parser`] reasons about. `literal` is a [`Cow`] because a dialect loaded
/// at runtime cannot produce a `&'static str`.
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

    /// A control token whose literal was discovered at runtime: read out of a GGUF's
    /// token table, or out of a config file.
    pub fn owned(role: ControlRole, literal: impl Into<String>) -> Self {
        ControlToken {
            role,
            literal: Cow::Owned(literal.into()),
        }
    }
}

/// The structural boundaries a parser can recognise: each is one vocab entry some model spells, so
/// each must be a [`RenderSpan::Control`]. `Ord` is derived so a diagnostic can sort the roles it
/// lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControlRole {
    BeginOfText,
    /// GLM's `<sop>`: the start-of-prompt token that follows `[gMASK]`. One vocab
    /// entry with no turn semantics, and not a `BeginOfText` - both are emitted.
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
    /// `<arg_key>`: GLM writes tool-call arguments as flat key/value pairs rather than
    /// JSON, and each delimiter is one vocab entry.
    ArgKeyOpen,
    ArgKeyClose,
    ArgValueOpen,
    ArgValueClose,
    ImageOpen,
    Image,
    ImageClose,
    EndOfTurn,
    /// A single-entry token with no structural role this contract models. Named rather than carried
    /// under an unrelated variant, so `all_with_role` lists the tokens nobody has modelled.
    Other,
}

/// Every control token a dialect uses, so the token core resolves them once at startup and fails
/// loudly if the vocab does not hold one. A slice, because an `Option` per role invites a renderer
/// to skip one it should have emitted.
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

    /// The primary lookup: a literal is what the template emits and what the vocab resolves to one
    /// id. Two entries may share a literal under different roles, which is legal, so this returns
    /// the first.
    pub fn by_literal(&self, literal: &str) -> Option<&ControlToken> {
        self.0.iter().find(|t| t.literal == literal)
    }

    /// Every token carrying a role: an iterator rather than the first match, so "this role has three
    /// literals" is a thing the caller must look at.
    pub fn all_with_role(&self, role: ControlRole) -> impl Iterator<Item = &ControlToken> {
        self.0.iter().filter(move |t| t.role == role)
    }
}

/// A literal that ends generation, with the role it plays: a stop token that is not one vocab entry
/// never fires, so the role lets the startup failure name what was lost.
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

/// What `parse` recovers from a decoded token stream: `parse` after `render` must be the identity on
/// the round-trippable parts. Each span carries the ids it came from, since only the parser walks
/// them.
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

/// The vocabulary, as much as a parser may know: `decode` takes a slice, because detokenizing a run
/// and joining per-token pieces differ on a BPE vocabulary.
pub trait TokenDecoder {
    /// Text for a run of ids. Infallible on purpose: dropping bytes is the failure this
    /// design exists to abolish, so an implementation that cannot decode something must
    /// render a visible marker instead.
    fn decode(&self, tokens: &[u32]) -> String;

    /// The role of a single id, if it is one of this dialect's control tokens: by id, because text
    /// that merely spells `</think>` is several ids and can never be the boundary.
    fn control_role(&self, token: u32) -> Option<ControlRole>;
}

/// How a mid-session system-prompt change is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemUpdateMode {
    /// The model reads the latest `system` message wherever it sits, so a change is
    /// appended after the cached history. Rewriting message 0 instead would cost a full
    /// cold re-prefill of a long conversation.
    InHistory,
    /// The model only honours a system prompt at position 0, so a change forces a
    /// re-render. A dialect declaring this is declaring a known, priced cost.
    Envelope,
}

/// A declared, model-specific mitigation. Declarative because the dialect says what to watch for and
/// the engine owns what to do, so guards stay testable without a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Stop when the same token repeats `run` times consecutively.
    RepetitionRun { run: u32 },
    /// Stop when an n-gram of `window` tokens repeats `times` times.
    RepetitionNgram { window: u32, times: u32 },
    /// Reasoning ran past `tokens` without producing a content or tool-call span.
    /// Not a cap on thinking: long thinking is fine, silent non-termination is not.
    ReasoningStall { tokens: u32 },
}

/// Which wire field a model replays its own reasoning into. A per-model fact, so [`DialectSpec`]
/// carries it; `letibot-transcript` mirrors the enum and `crates/turn` maps between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningField {
    /// GLM: replayed as `reasoning_content`.
    ReasoningContent,
    /// Qwen: replayed inside the rendered turn, gated by `preserve_thinking`.
    Inline,
}

/// Everything about a model that rendering needs, as a value. There are no methods because one
/// template engine serves every model, so a new model needs no code.
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

/// Token ids in, spans out. Parsing is not data because it is not a template: the shipped jinja says
/// how a turn is written, never how to read one back.
pub trait Parser: Send + Sync {
    /// Takes `u32` to keep this crate free of the FFI, and the decoder because it must.
    ///
    /// `reasoning_open` is the channel the model speaks on before the first token: a generation
    /// prompt commonly leaves the reasoning block open, and a stateless parse would promote a
    /// cut-off deliberation to visible content.
    fn parse(
        &self,
        tokens: &[u32],
        decoder: &dyn TokenDecoder,
        reasoning_open: bool,
    ) -> Vec<ParsedSpan>;
}

/// Concatenate the text of a span sequence. The detokenized form of a render must equal the shipped
/// template's output exactly, and this is the "ours" half of that comparison.
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

/// The same spans as the string a server tokenizes itself, for a prompt carrying media.
///
/// Images travel only as a `prompt_string`, which the server tokenizes and marks per process. That
/// is safe for the cache while control tokens stay atomic, which is this file's job to keep. One
/// image is one marker: the opening role places it and the other two are swallowed.
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

/// How many images a span sequence places, one per `ImageOpen`: the engine pairs this with the media
/// it sends, because the server substitutes pictures in the order the markers appear.
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
        // One role, three literals: the case a lookup by role would answer one of.
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

    /// Why `Cow` is not cosmetic: every string here is owned, as it would be coming out
    /// of a GGUF's template and token table at runtime, which a `&'static str` could not
    /// express without leaking.
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
