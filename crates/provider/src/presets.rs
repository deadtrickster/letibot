//! The providers, as facts about their doors.

use crate::effort::Effort;

/// Which protocol a door speaks: the shape of the request body and of the stream. A preset says
/// which, and that decides which client builds the session's backend - one match arm in
/// `harness::open::build_provider`, so another can arrive without touching any client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// OpenAI chat completions: `POST …/chat/completions`, `data:` chunks, `[DONE]`.
    ChatCompletions,
    /// Anthropic Messages: `POST …/v1/messages`, `anthropic-version`, named SSE events.
    AnthropicMessages,
    /// OpenAI Responses: `POST …/responses`, input items, and a reasoning item per thought.
    Responses,
    /// Google GenAI: `POST …/models/{model}:streamGenerateContent`, `contents` of parts, and the
    /// model in the path, so the preset's URL is a base.
    GenAi,
}

/// One provider's shape: where it is, which header carries the key, and which
/// environment variable the operator will have put the key in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub url: &'static str,
    pub key_env: &'static str,
    /// What the request body and the stream look like on this door.
    pub protocol: Protocol,
    /// Other names the same key goes by (a vendor's SDK and its rebrand).
    pub alt_envs: &'static [&'static str],
    /// The fallback for a box with no catalogue. The API silently aliases retired names onto a
    /// live model, so this must be one the account's `GET /models` offers.
    pub fallback_model: &'static str,
    /// Extra body fields the provider needs to think out loud, if any.
    pub thinking_field: Option<&'static str>,
    /// This provider's id in the models.dev catalogue, which is not always the name we
    /// call it by: we say `glm` and `grok`, the catalogue says `zhipuai` and `xai`. The
    /// facts are read from there rather than written down here.
    pub catalogue_id: &'static str,
    /// The catalogue's rule (window, price, plainest name) cannot see a vendor's flagship. When
    /// set, `fallback_model` is the default wherever the catalogue still carries it.
    pub vendor_pick: bool,
    /// Sampling keys this door fixes and refuses any other value for. A configured one is
    /// dropped before the request, and the operator is told once.
    pub fixed_sampling: &'static [&'static str],
    /// The top-level body field that carries `--effort`, and the door's own words for it.
    pub effort: Option<EffortField>,
    /// Where this door was defined, for the listings that mark config and env doors.
    pub origin: Origin,
    /// The window the operator declared, for a door no catalogue carries.
    pub window: Option<u64>,
    /// Where the plan reports its windows' resets ([`crate::usage`]): a path on the door's own
    /// origin, or a whole URL. Asked only for a limit whose refusal named no instant.
    pub usage_url: Option<&'static str>,
    /// How this door's request body differs from plain chat completions.
    pub compat: Compat,
}

/// Where a door came from: compiled in, changed or added by providers.toml, or an env base URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    BuiltIn,
    /// A built-in with a `[provider.<name>]` entry over it.
    Changed,
    /// A `[provider.<name>]` entry naming no built-in.
    Config,
    /// Defined by this base-URL variable.
    Env(&'static str),
}

impl Origin {
    /// The mark a listing draws beside the door's name.
    pub fn label(&self) -> String {
        match self {
            Origin::BuiltIn => "built-in".into(),
            Origin::Changed => "built-in, changed in providers.toml".into(),
            Origin::Config => "providers.toml".into(),
            Origin::Env(var) => format!("${var}"),
        }
    }
}

/// How a door spends `--effort`: the levels it takes and the body field that carries one.
/// `field: None` is a door whose only switch is thinking on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffortField {
    pub field: Option<&'static str>,
    /// The scale's words this door takes; `max` alone stands for "thinking on".
    pub levels: &'static [Effort],
}

/// Whether past `reasoning_content` goes back on assistant messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEcho {
    /// When the request carries tools: DeepSeek's thinking-with-tools contract.
    Tools,
    Always,
    Never,
}

/// How tool schemas are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSchema {
    Plain,
    /// Every `enum` node gets a `type`, inferred from its values: Kimi refuses an untyped one.
    TypedEnum,
}

/// A door's request quirks, as data, so a `[provider.NAME]` entry can declare them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compat {
    /// The body field the reply cap goes in.
    pub max_tokens_field: &'static str,
    pub reasoning_echo: ReasoningEcho,
    /// An assistant tool-call message must carry `reasoning_content`, empty if none was recorded.
    pub reasoning_required: bool,
    pub tool_schema: ToolSchema,
    /// The door takes `response_format: {"type": "json_object"}`.
    pub json_object: bool,
    /// Send `prompt_cache_key`, one per provider instance.
    pub cache_key: bool,
    /// The endpoint admits clients by `User-Agent`: send ours, and explain a 403 as that gate.
    pub client_gate: bool,
    /// Models (by substring of the id) that refuse `thinking: disabled`: the field is left out.
    pub thinking_locked: &'static [&'static str],
    /// This door carries a document and a picture in one request. Z.ai documents the opposite for
    /// GLM - file parts are not mixed with image/video parts - so a GLM door sets this false and a
    /// request holding a picture sends its PDF as text.
    pub pdf_beside_images: bool,
}

impl Compat {
    pub const PLAIN: Compat = Compat {
        max_tokens_field: "max_tokens",
        reasoning_echo: ReasoningEcho::Tools,
        reasoning_required: false,
        tool_schema: ToolSchema::Plain,
        json_object: false,
        cache_key: false,
        client_gate: false,
        thinking_locked: &[],
        pdf_beside_images: true,
    };

    /// Whether `model` may be sent `thinking: disabled` on this door.
    pub fn may_switch_thinking_off(&self, model: &str) -> bool {
        !self.thinking_locked.iter().any(|m| model.contains(m))
    }
}

const JSON_COMPAT: Compat = Compat {
    json_object: true,
    ..Compat::PLAIN
};

/// Zhipu's doors, which say so themselves: "File parts are only supported for the GLM-5.3-Flash
/// series ... and must not be mixed with image_url or video_url in the same request"
/// (<https://docs.z.ai/api-reference/llm/chat-completion.md>).
const GLM_COMPAT: Compat = Compat {
    json_object: true,
    pdf_beside_images: false,
    ..Compat::PLAIN
};

const KIMI_COMPAT: Compat = Compat {
    reasoning_echo: ReasoningEcho::Always,
    reasoning_required: true,
    tool_schema: ToolSchema::TypedEnum,
    cache_key: true,
    ..KIMI_GATE
};

/// Kimi's plan admits coding clients by `User-Agent`, and K3.1 Pro answers `disabled` with a 400.
const KIMI_GATE: Compat = Compat {
    client_gate: true,
    thinking_locked: &["3.1"],
    ..Compat::PLAIN
};

/// DeepSeek's chat door: `thinking` on or off, and `reasoning_effort` low/high/max while on.
const DEEPSEEK_EFFORT: EffortField = EffortField {
    field: Some("reasoning_effort"),
    levels: &[Effort::Off, Effort::Low, Effort::High, Effort::Max],
};

/// Anthropic's `output_config.effort`; `off` sends no `thinking` and no level.
/// <https://platform.claude.com/docs/en/build-with-claude/effort>
const ANTHROPIC_EFFORT: EffortField = EffortField {
    field: Some("output_config.effort"),
    levels: &Effort::ALL,
};

/// GLM switches thinking on or off and has no level.
const GLM_EFFORT: EffortField = EffortField {
    field: None,
    levels: &[Effort::Off, Effort::Max],
};

/// Kimi's plan answers its 5-hour and monthly windows here, on the door's own host and key.
const KIMI_USAGES: &str = "/coding/v1/usages";

/// Kimi's plan fixes these on every model it serves; another value is a 400.
const KIMI_FIXED_SAMPLING: &[&str] = &[
    "temperature",
    "top_p",
    "n",
    "presence_penalty",
    "frequency_penalty",
];

/// The plan's `reasoning_effort` takes low/high/max.
const KIMI_EFFORT: EffortField = EffortField {
    field: Some("reasoning_effort"),
    levels: &[Effort::Off, Effort::Low, Effort::High, Effort::Max],
};

pub const DEEPSEEK: Preset = Preset {
    name: "deepseek",
    url: "https://api.deepseek.com/chat/completions",
    key_env: "DEEPSEEK_API_KEY",
    protocol: Protocol::ChatCompletions,
    alt_envs: &[],
    fallback_model: "deepseek-flash",
    thinking_field: Some("thinking"),
    catalogue_id: "deepseek",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: Some(DEEPSEEK_EFFORT),
    origin: Origin::BuiltIn,
    window: None,
    usage_url: None,
    compat: JSON_COMPAT,
};

pub const GLM: Preset = Preset {
    name: "glm",
    url: "https://open.bigmodel.cn/api/paas/v4/chat/completions",
    key_env: "ZHIPUAI_API_KEY",
    protocol: Protocol::ChatCompletions,
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    fallback_model: "glm-5.3-flash",
    // Zhipu's `thinking: {"type": "enabled"}` switches GLM's reasoning on; the
    // provider sends it when the operator asks for a reasoning turn.
    thinking_field: Some("thinking"),
    catalogue_id: "zhipuai",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: Some(GLM_EFFORT),
    origin: Origin::BuiltIn,
    window: None,
    usage_url: None,
    compat: GLM_COMPAT,
};

/// The coding plan: same vendor, a different path (`/api/coding/paas/v4`), its own model list and
/// zero prices. A preset, not a base-URL override, so the window and default follow it; its key on
/// the metered URL reads as an empty account (a 429).
pub const GLM_CODING: Preset = Preset {
    name: "glm-coding",
    url: "https://api.z.ai/api/coding/paas/v4/chat/completions",
    key_env: "ZHIPUAI_API_KEY",
    protocol: Protocol::ChatCompletions,
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    // The catalogue's rule picks this: the plan's models tie on window and price, so the
    // qualifier and version rungs decide, and they name glm-5.3.
    fallback_model: "glm-5.3",
    thinking_field: Some("thinking"),
    catalogue_id: "zai-coding-plan",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: Some(GLM_EFFORT),
    origin: Origin::BuiltIn,
    window: None,
    usage_url: Some(crate::usage::ZAI_QUOTA),
    compat: GLM_COMPAT,
};

/// Zhipu's coding plan: the mainland host of the same product as [`GLM_CODING`].
pub const GLM_CODING_CN: Preset = Preset {
    name: "glm-coding-cn",
    url: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
    key_env: "ZHIPUAI_API_KEY",
    protocol: Protocol::ChatCompletions,
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    fallback_model: "glm-5.3",
    thinking_field: Some("thinking"),
    catalogue_id: "zhipuai-coding-plan",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: Some(GLM_EFFORT),
    origin: Origin::BuiltIn,
    window: None,
    usage_url: Some(crate::usage::ZAI_QUOTA),
    compat: GLM_COMPAT,
};

pub const GROK: Preset = Preset {
    name: "grok",
    url: "https://api.x.ai/v1/chat/completions",
    key_env: "XAI_API_KEY",
    protocol: Protocol::ChatCompletions,
    alt_envs: &["GROK_API_KEY"],
    fallback_model: "grok-4.3",
    thinking_field: None,
    catalogue_id: "xai",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: None,
    origin: Origin::BuiltIn,
    window: None,
    usage_url: None,
    compat: JSON_COMPAT,
};

/// Anthropic's own API, speaking Messages, keyed by `x-api-key`. `ANTHROPIC_AUTH_TOKEN` is not an
/// alias: it is an OAuth bearer for a different header. The fallback model is the one the models
/// overview recommends, so the preset works without a catalogue.
pub const ANTHROPIC: Preset = Preset {
    name: "anthropic",
    url: "https://api.anthropic.com/v1/messages",
    key_env: "ANTHROPIC_API_KEY",
    protocol: Protocol::AnthropicMessages,
    alt_envs: &[],
    fallback_model: "claude-opus-5-5",
    thinking_field: None,
    catalogue_id: "anthropic",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: Some(ANTHROPIC_EFFORT),
    origin: Origin::BuiltIn,
    window: None,
    usage_url: None,
    compat: Compat::PLAIN,
};

/// DeepSeek's Anthropic-compatible door at `/anthropic`: the same models as [`DEEPSEEK`], so the
/// catalogue id is `deepseek`'s and a turn is priced by model, not URL.
/// <https://api-docs.deepseek.com/guides/anthropic_api>
pub const DEEPSEEK_ANTHROPIC: Preset = Preset {
    name: "deepseek-anthropic",
    url: "https://api.deepseek.com/anthropic/v1/messages",
    key_env: "DEEPSEEK_API_KEY",
    protocol: Protocol::AnthropicMessages,
    alt_envs: &[],
    fallback_model: "deepseek-flash",
    thinking_field: None,
    catalogue_id: "deepseek",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: None,
    origin: Origin::BuiltIn,
    window: None,
    usage_url: None,
    compat: Compat::PLAIN,
};

/// Z.ai's Anthropic-compatible door, the one their Claude Code instructions use: the same
/// coding-plan models as [`GLM_CODING`]. <https://docs.z.ai/scenario-example/develop-tools/claude>
pub const GLM_ANTHROPIC: Preset = Preset {
    name: "glm-anthropic",
    url: "https://api.z.ai/api/anthropic/v1/messages",
    key_env: "ZHIPUAI_API_KEY",
    protocol: Protocol::AnthropicMessages,
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    fallback_model: "glm-5.3",
    thinking_field: None,
    catalogue_id: "zai-coding-plan",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: None,
    origin: Origin::BuiltIn,
    window: None,
    usage_url: None,
    compat: Compat::PLAIN,
};

/// Moonshot's Kimi Code plan on Messages. The URL is the whole endpoint, since this client posts
/// what it is given. Not `ANTHROPIC_API_KEY`, so one box's two keys never cross; priced and
/// windowed as [`KIMI_CODE_GLOBAL`].
pub const KIMI_CODING: Preset = Preset {
    name: "kimi-coding",
    url: "https://api.kimi.ai/coding/v1/messages",
    key_env: "KIMI_API_KEY",
    protocol: Protocol::AnthropicMessages,
    // Not `MOONSHOT_API_KEY`: that is the metered Open Platform's key, another product's.
    alt_envs: &["KIMI_CODE_API_KEY"],
    fallback_model: "k3.1-pro",
    thinking_field: None,
    catalogue_id: "kimi-code-plan-global",
    vendor_pick: true,
    fixed_sampling: KIMI_FIXED_SAMPLING,
    effort: None,
    origin: Origin::BuiltIn,
    window: None,
    usage_url: Some(KIMI_USAGES),
    compat: KIMI_GATE,
};

/// OpenAI itself, on its own protocol. `gpt-6-astra` is the id the API reference's examples use;
/// the catalogue has no `openai` row, so a box with no operator file has only this to go on.
pub const OPENAI: Preset = Preset {
    name: "openai",
    url: "https://api.openai.com/v1/responses",
    key_env: "OPENAI_API_KEY",
    protocol: Protocol::Responses,
    alt_envs: &[],
    fallback_model: "gpt-6-astra",
    // The Responses API switches thinking with `reasoning.effort`, not a field of this
    // table's shape: `openai::thinking_field` is the chat-completions spelling.
    thinking_field: None,
    catalogue_id: "openai",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: None,
    origin: Origin::BuiltIn,
    window: None,
    usage_url: None,
    compat: Compat::PLAIN,
};

/// Google's Gemini: the model is in the path, so [`Preset::url`] is a base; [`crate::gemini`] is
/// the client. The fallback is Google's everyday-coding model. The vendored catalogue has no
/// `google` row yet, so without another catalogue nothing is priced or windowed.
pub const GEMINI: Preset = Preset {
    name: "gemini",
    url: "https://generativelanguage.googleapis.com/v1beta",
    key_env: "GEMINI_API_KEY",
    protocol: Protocol::GenAi,
    alt_envs: &["GOOGLE_API_KEY"],
    fallback_model: "gemini-4-hydrogen",
    thinking_field: None,
    catalogue_id: "google",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: None,
    origin: Origin::BuiltIn,
    window: None,
    usage_url: None,
    compat: Compat::PLAIN,
};

/// Kimi's coding plan (Moonshot): chat completions on the plan's own host. Not the pay-per-token
/// Open Platform (`api.moonshot.ai/v1`): the two products' keys and URLs are not interchangeable.
/// <https://www.kimi.com/code/docs/kimi-code/models.html>
pub const KIMI_CODE: Preset = Preset {
    name: "kimi-code",
    url: "https://api.kimi.com/coding/v1/chat/completions",
    key_env: "KIMI_API_KEY",
    protocol: Protocol::ChatCompletions,
    alt_envs: &["KIMI_CODE_API_KEY"],
    fallback_model: "k3.1-pro",
    thinking_field: Some("thinking"),
    catalogue_id: "kimi-code-plan-cn",
    vendor_pick: true,
    fixed_sampling: KIMI_FIXED_SAMPLING,
    effort: Some(KIMI_EFFORT),
    origin: Origin::BuiltIn,
    window: None,
    usage_url: Some(KIMI_USAGES),
    compat: KIMI_COMPAT,
};

/// The same plan on the global host. One key works on one region's host only, so this is a
/// preset of its own rather than a base-URL override: the window follows the catalogue id.
pub const KIMI_CODE_GLOBAL: Preset = Preset {
    name: "kimi-code-global",
    url: "https://api.kimi.ai/coding/v1/chat/completions",
    key_env: "KIMI_API_KEY",
    protocol: Protocol::ChatCompletions,
    alt_envs: &["KIMI_CODE_API_KEY"],
    fallback_model: "k3.1-pro",
    thinking_field: Some("thinking"),
    catalogue_id: "kimi-code-plan-global",
    vendor_pick: true,
    fixed_sampling: KIMI_FIXED_SAMPLING,
    effort: Some(KIMI_EFFORT),
    origin: Origin::BuiltIn,
    window: None,
    usage_url: Some(KIMI_USAGES),
    compat: KIMI_COMPAT,
};

pub const ALL: &[&Preset] = &[
    &DEEPSEEK,
    &GLM,
    &GLM_CODING,
    &GLM_CODING_CN,
    &GROK,
    &ANTHROPIC,
    &DEEPSEEK_ANTHROPIC,
    &GLM_ANTHROPIC,
    &OPENAI,
    &GEMINI,
    &KIMI_CODE,
    &KIMI_CODE_GLOBAL,
    // After the vendor's own doors, as each `-anthropic` door is: the order is which door a
    // shared catalogue's row is reached by first.
    &KIMI_CODING,
];

impl Preset {
    /// The context window to plan compaction against, from the catalogue; `None` when it
    /// has no figure for this model. `None` means the caller leaves the window alone,
    /// because a window invented here would decide when a conversation is summarised.
    pub fn window(&self, model: Option<&str>, cat: &crate::catalogue::Catalogue) -> Option<u64> {
        let model = match model {
            Some(m) => m.to_string(),
            None => self.default_model(cat),
        };
        cat.model(self.catalogue_id, &model)
            .map(|m| m.context)
            .or(self.window)
    }

    /// The model card's line for one model: the catalogue's facts, else what the operator
    /// declared. Missing facts are said as missing, never as free or as some default size.
    pub fn facts_line(
        &self,
        model: &str,
        cat: &crate::catalogue::Catalogue,
        prices: &std::collections::BTreeMap<String, Prices>,
    ) -> String {
        if let Some(f) = cat.model(self.catalogue_id, model) {
            return f.card_line();
        }
        if self.origin == Origin::BuiltIn {
            return crate::catalogue::ModelFacts::missing_line().to_string();
        }
        let window = match self.window {
            Some(w) => format!("{} window", crate::catalogue::tokens(w)),
            None => "window unknown".to_string(),
        };
        let price = match prices.get(model) {
            Some(p) => format!("${} in / ${} out per M", p.input, p.output),
            None => "unpriced".to_string(),
        };
        format!("{window} · {price} · {}", self.origin.label())
    }

    /// The model to use when the operator names none: the catalogue's pick by
    /// rule, else this build's frozen fallback. See [`Preset::fallback_model`].
    pub fn default_model(&self, cat: &crate::catalogue::Catalogue) -> String {
        self.pick(cat).0
    }

    /// [`Preset::default_model`] with the rung that chose it, so a reader sees why: the vendor's
    /// own pick, the catalogue's rule, or the fallback frozen into this build.
    pub fn pick(&self, cat: &crate::catalogue::Catalogue) -> (String, String) {
        if self.vendor_pick && cat.model(self.catalogue_id, self.fallback_model).is_some() {
            return (
                self.fallback_model.to_string(),
                "the vendor's own pick for this door".to_string(),
            );
        }
        match cat.pick(self.catalogue_id) {
            Some(p) => (p.model, p.why),
            None => (
                self.fallback_model.to_string(),
                "this build's frozen fallback (the catalogue carries no model for this door)"
                    .to_string(),
            ),
        }
    }

    /// Remove the keys this door fixes from `sampling`, returning each as `key=value`.
    pub fn drop_fixed_sampling(
        &self,
        sampling: &mut serde_json::Map<String, serde_json::Value>,
    ) -> Vec<String> {
        self.fixed_sampling
            .iter()
            .filter_map(|k| sampling.remove(*k).map(|v| format!("{k}={v}")))
            .collect()
    }

    /// The field that switches thinking on and off for one request.
    pub fn thinking_off_field(&self) -> Option<&'static str> {
        self.thinking_field
    }

    /// Whether this door takes `response_format: {"type": "json_object"}`.
    pub fn json_object(&self) -> bool {
        self.protocol == Protocol::ChatCompletions && self.compat.json_object
    }

    /// The level this door sends for `asked`, and the sentence to say when that is not `asked`.
    /// `None` sends nothing: the door has no switch, or no rung on that side of `off`.
    pub fn effort_level(&self, asked: Effort) -> (Option<Effort>, Option<String>) {
        let Some(e) = self.effort else {
            return (
                None,
                Some(format!(
                    "`{}` has no effort switch; effort `{asked}` is not sent",
                    self.name
                )),
            );
        };
        let have = || {
            e.levels
                .iter()
                .map(|l| l.word())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match asked.clamp_to(e.levels) {
            Some(l) if l == asked => (Some(l), None),
            Some(l) => (
                Some(l),
                Some(format!(
                    "`{}` takes effort {}; `{asked}` is sent as `{l}`",
                    self.name,
                    have()
                )),
            ),
            None => (
                None,
                Some(format!(
                    "`{}` takes effort {}; `{asked}` is not sent",
                    self.name,
                    have()
                )),
            ),
        }
    }

    /// `--effort` as this door's body field and word, or `None` where it has no field, no rung,
    /// or the level is `off`: the model's own default then stands.
    pub fn effort_for(&self, effort: Option<&str>) -> Option<(&'static str, &'static str)> {
        let asked = Effort::parse(effort?).ok()?;
        let field = self.effort?.field?;
        match self.effort_level(asked).0? {
            Effort::Off => None,
            l => Some((field, l.word())),
        }
    }

    /// Names this build knows, with the aliases each answers to; anything else is an error naming
    /// every one of them rather than a guess - read off [`ALL`], so adding a door does not have to
    /// find this sentence.
    pub fn parse(s: &str) -> Result<&'static Preset, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "deepseek" => Ok(&DEEPSEEK),
            "glm" | "zhipu" | "bigmodel" | "z.ai" => Ok(&GLM),
            // Named separately because reaching for the subscription under the metered
            // name answers a 429 that reads like an empty account. See [`GLM_CODING`].
            "glm-coding" | "zai-coding" | "coding-plan" => Ok(&GLM_CODING),
            "glm-coding-cn" | "zhipu-coding" | "bigmodel-coding" => Ok(&GLM_CODING_CN),
            "grok" | "xai" | "x.ai" => Ok(&GROK),
            // The same vendors over the Messages wire. Their own names, because a base URL moved
            // onto another protocol is a different door: one answers `tool_result` and the other
            // `tool` messages, and a preset is what that choice is written down as.
            "anthropic" | "claude" => Ok(&ANTHROPIC),
            "deepseek-anthropic" | "deepseek-messages" => Ok(&DEEPSEEK_ANTHROPIC),
            "glm-anthropic" | "zai-anthropic" | "glm-messages" => Ok(&GLM_ANTHROPIC),
            // The plan over Messages. `moonshot` alone would be a guess between the plan and the
            // metered Open Platform, and the two keys are not interchangeable.
            "kimi-coding" | "kimi-anthropic" => Ok(&KIMI_CODING),
            "kimi-code" | "kimi" | "moonshot-coding" => Ok(&KIMI_CODE),
            "kimi-code-global" | "kimi-global" => Ok(&KIMI_CODE_GLOBAL),
            "openai" => Ok(&OPENAI),
            "gemini" | "google" | "genai" => Ok(&GEMINI),
            // Read from `ALL` rather than typed again: a provider added to one list and not
            // the other is exactly the drift this sentence exists to prevent.
            other => Err(format!(
                "`{other}` is not a provider this build knows; there are {}: {}",
                ALL.len(),
                ALL.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

/// USD per million tokens, for one model. From the operator's file, never from
/// memory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prices {
    pub input: f64,
    pub cached: f64,
    pub output: f64,
}

impl Prices {
    /// Micro-dollars for one turn.
    pub fn micros(&self, prompt: u64, cached: u64, generated: u64) -> u64 {
        let uncached = prompt.saturating_sub(cached) as f64;
        let usd =
            (uncached * self.input + cached as f64 * self.cached + generated as f64 * self.output)
                / 1_000_000.0;
        (usd * 1_000_000.0).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_price_table_counts_cached_tokens_at_the_cached_rate() {
        let p = Prices {
            input: 1.0,
            cached: 0.1,
            output: 2.0,
        };
        // 1000 prompt of which 600 cached, 100 out: 400*1 + 600*0.1 + 100*2 = 660 micro-USD.
        assert_eq!(p.micros(1000, 600, 100), 660);
        assert_eq!(Preset::parse("Zhipu").unwrap().name, "glm");
        assert_eq!(Preset::parse("openai").unwrap().name, "openai");
        // An unknown name names every provider this build knows, which is how a reader
        // discovers a door they did not know was there.
        let why = Preset::parse("nonesuch").unwrap_err();
        for p in ALL {
            assert!(why.contains(p.name), "{why}");
        }
        assert!(why.contains(&format!("there are {}", ALL.len())), "{why}");
        // Every door has to be reachable by its own name, or `ALL` and `parse` disagree about what
        // this build knows and a picker offers a name the flag refuses.
        for preset in ALL {
            assert_eq!(Preset::parse(preset.name).map(|p| p.name), Ok(preset.name));
        }
        // A door's protocol is what decides which client answers for it.
        assert_eq!(ANTHROPIC.protocol, Protocol::AnthropicMessages);
        assert_eq!(DEEPSEEK.protocol, Protocol::ChatCompletions);
        assert_eq!(OPENAI.protocol, Protocol::Responses);
        assert_eq!(GEMINI.protocol, Protocol::GenAi);
    }

    /// The plan's two hosts, its own key and its thinking switch: the facts a reader of the
    /// table above cannot check without calling Moonshot with a key.
    #[test]
    fn the_kimi_presets_are_the_coding_plans_two_hosts() {
        assert_eq!(
            KIMI_CODE.url,
            "https://api.kimi.com/coding/v1/chat/completions"
        );
        assert_eq!(
            KIMI_CODE_GLOBAL.url,
            "https://api.kimi.ai/coding/v1/chat/completions"
        );
        for p in [&KIMI_CODE, &KIMI_CODE_GLOBAL] {
            assert_eq!(p.key_env, "KIMI_API_KEY", "{}", p.name);
            assert_eq!(p.alt_envs, &["KIMI_CODE_API_KEY"], "{}", p.name);
            assert_eq!(p.fallback_model, "k3.1-pro", "{}", p.name);
            assert_eq!(p.thinking_field, Some("thinking"), "{}", p.name);
            assert_eq!(p.effort.and_then(|e| e.field), Some("reasoning_effort"));
            // The metered Open Platform's key is a different product's.
            assert!(!p.key_env.contains("MOONSHOT"), "{}", p.name);
        }
        assert_eq!(Preset::parse("kimi").unwrap().name, "kimi-code");
        assert_eq!(Preset::parse("Moonshot-Coding").unwrap().name, "kimi-code");
        assert_eq!(
            Preset::parse("kimi-code-global").unwrap().name,
            "kimi-code-global"
        );
    }

    /// A vendor's own door comes before its Messages door: the first door of a shared catalogue
    /// names the model card's row and is the one its models are taken through.
    #[test]
    fn a_vendors_own_door_precedes_its_messages_door() {
        let order: Vec<&str> = ALL.iter().map(|p| p.name).collect();
        let pos = |n: &str| order.iter().position(|x| *x == n).unwrap();
        assert!(pos("kimi-code-global") < pos("kimi-coding"), "{order:?}");
        assert!(pos("deepseek") < pos("deepseek-anthropic"), "{order:?}");
        assert!(pos("glm") < pos("glm-anthropic"), "{order:?}");
    }

    /// Every `fallback_model` is the catalogue's shortest name, so boxes with and without a
    /// catalogue choose the same. `openai` is absent: the vendored snapshot has no section for it,
    /// and its fallback is the id the API reference's examples send.
    #[test]
    fn every_fallback_names_a_real_model_and_its_shortest_form() {
        let cat = crate::catalogue::Catalogue::snapshot();
        assert!(
            !cat.is_empty(),
            "the vendored snapshot parses, or every fallback name is unchecked"
        );
        for (preset, catalogue_id) in [
            (&DEEPSEEK, "deepseek"),
            (&GLM, "zhipuai"),
            (&GLM_CODING, "zai-coding-plan"),
            (&GLM_CODING_CN, "zhipuai-coding-plan"),
            (&GROK, "xai"),
            (&KIMI_CODE, "kimi-code-plan-cn"),
            (&KIMI_CODE_GLOBAL, "kimi-code-plan-global"),
            (&KIMI_CODING, "kimi-code-plan-global"),
        ] {
            let Some(facts) = cat.model(catalogue_id, preset.fallback_model) else {
                panic!(
                    "{}: fallback_model `{}` is not a model {} carries — this is the \
                     failure that shipped once already (`deepseek-chat`, `grok-4-fast`). \
                     Pick one of: {:?}",
                    preset.name,
                    preset.fallback_model,
                    catalogue_id,
                    cat.model_names(catalogue_id),
                );
            };
            // A vendor's pick is the default on both boxes, so a shorter name with the same
            // facts cannot make them disagree. That is what the check below protects.
            if preset.vendor_pick {
                assert_eq!(preset.default_model(&cat), preset.fallback_model);
                continue;
            }
            // No shorter name may name the same window and price, or the fallback is a
            // longer spelling of what the catalogue's own rule already picks.
            if let Some(shorter) =
                cat.shorter_equivalent(catalogue_id, preset.fallback_model, facts)
            {
                panic!(
                    "{}: fallback_model `{}` is a longer name for the same model — `{}` has \
                     the same window and price and is what the catalogue's rule picks. A box \
                     with a catalogue and a box without one would choose differently.",
                    preset.name, preset.fallback_model, shorter,
                );
            }
        }
    }

    /// The plan's model ids, as its models page lists them, on both catalogue ids the Kimi doors
    /// read: the card offers what the catalogue carries.
    #[test]
    fn the_kimi_catalogue_carries_the_plans_model_ids() {
        let cat = crate::catalogue::Catalogue::snapshot();
        for id in ["kimi-code-plan-cn", "kimi-code-plan-global"] {
            let names = cat.model_names(id);
            for m in [
                "k3.1-pro",
                "k3.1-pro-highspeed",
                "k3",
                "k3-256k",
                "kimi-for-coding",
                "kimi-for-coding-highspeed",
            ] {
                assert!(names.contains(&m.to_string()), "{id} lacks {m}: {names:?}");
            }
        }
        for p in [&KIMI_CODE, &KIMI_CODE_GLOBAL, &KIMI_CODING] {
            assert_eq!(p.default_model(&cat), "k3.1-pro", "{}", p.name);
        }
    }

    /// A fixed key is dropped and named; a key the door leaves free stays.
    #[test]
    fn the_kimi_doors_drop_the_sampling_they_fix() {
        let mut s =
            serde_json::json!({"temperature": 0.7, "top_p": 0.9, "seed": 7, "max_tokens": 9})
                .as_object()
                .unwrap()
                .clone();
        let dropped = KIMI_CODE_GLOBAL.drop_fixed_sampling(&mut s);
        assert_eq!(dropped, ["temperature=0.7", "top_p=0.9"]);
        assert_eq!(s.keys().collect::<Vec<_>>(), ["max_tokens", "seed"]);
        let mut d = serde_json::json!({"temperature": 0.7})
            .as_object()
            .unwrap()
            .clone();
        assert!(DEEPSEEK.drop_fixed_sampling(&mut d).is_empty());
        assert!(d.contains_key("temperature"));
    }

    #[test]
    fn the_kimi_effort_is_low_high_or_max() {
        let e = |w| KIMI_CODE.effort_for(Some(w)).map(|(_, v)| v);
        assert_eq!(e("low"), Some("low"));
        assert_eq!(e("medium"), Some("high"));
        assert_eq!(e("High"), Some("high"));
        assert_eq!(e("xhigh"), Some("max"));
        assert_eq!(e("bogus"), None);
        assert_eq!(e("off"), None);
        assert_eq!(KIMI_CODE.effort_for(None), None);
        assert_eq!(GLM.effort_for(Some("high")), None, "GLM has no level field");
        assert_eq!(GROK.effort_for(Some("high")), None);
    }

    /// A rung the door lacks is clamped and named; a door with no switch says nothing is sent.
    #[test]
    fn an_unsupported_effort_clamps_and_says_so() {
        assert_eq!(
            DEEPSEEK.effort_level(Effort::High),
            (Some(Effort::High), None)
        );
        let (l, why) = DEEPSEEK.effort_level(Effort::Medium);
        assert_eq!(l, Some(Effort::High));
        assert!(why.unwrap().contains("`medium` is sent as `high`"));
        let (l, why) = GLM.effort_level(Effort::Low);
        assert_eq!(l, Some(Effort::Max), "thinking on, never off");
        assert!(why.unwrap().contains("takes effort off, max"));
        assert_eq!(GLM.effort_level(Effort::Off), (Some(Effort::Off), None));
        let (l, why) = GROK.effort_level(Effort::High);
        assert_eq!(l, None);
        assert!(why.unwrap().contains("no effort switch"));
    }
}
