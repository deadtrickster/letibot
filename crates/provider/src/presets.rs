//! The providers, as facts about their doors.

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
    /// The fallback for a box with no catalogue: the API accepts a retired name and
    /// silently aliases it onto a live model, so this must be one the account offers.
    ///
    /// ```text
    /// GET /models            -> deepseek-flash, deepseek-v4-pro        (offered: 2)
    /// POST /chat/completions -> accepts deepseek-flash, deepseek-v4-flash,
    ///                           deepseek-v4-pro, deepseek-chat
    ///                           and answers with `model: deepseek-flash` for the two
    ///                           names it does not offer                  (accepted: 4)
    /// ```
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
}

/// An effort switch as one body field: `words` maps the operator's word to the door's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffortField {
    pub field: &'static str,
    pub words: &'static [(&'static str, &'static str)],
}

/// Kimi's plan fixes these on every model it serves; another value is a 400.
const KIMI_FIXED_SAMPLING: &[&str] = &[
    "temperature",
    "top_p",
    "n",
    "presence_penalty",
    "frequency_penalty",
];

/// The plan's `reasoning_effort` takes low/high/max, and its docs map the rest onto those.
const KIMI_EFFORT: EffortField = EffortField {
    field: "reasoning_effort",
    words: &[
        ("minimal", "low"),
        ("low", "low"),
        ("medium", "high"),
        ("high", "high"),
        ("xhigh", "max"),
        ("max", "max"),
    ],
};

pub const DEEPSEEK: Preset = Preset {
    name: "deepseek",
    url: "https://api.deepseek.com/chat/completions",
    key_env: "DEEPSEEK_API_KEY",
    protocol: Protocol::ChatCompletions,
    alt_envs: &[],
    fallback_model: "deepseek-flash",
    thinking_field: None,
    catalogue_id: "deepseek",
    vendor_pick: false,
    fixed_sampling: &[],
    effort: None,
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
    effort: None,
};

/// The coding plan: same vendor, a different path (`/api/coding/paas/v4`), its own
/// model list and zero prices. A preset and not a base-URL override, so the window and
/// the derived default follow it; its key on the metered URL reads as an empty account.
///
/// ```text
/// http 429: 余额不足或无可用资源包,请充值。
/// ```
///
/// ```text
/// zai                  https://api.z.ai/api/paas/v4              18 models
/// zai-coding-plan      https://api.z.ai/api/coding/paas/v4        7 models
/// zhipuai              https://open.bigmodel.cn/api/paas/v4      17 models
/// zhipuai-coding-plan  https://open.bigmodel.cn/api/coding/paas/v4 4 models
/// ```
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
    effort: None,
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
    effort: None,
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
};

/// Anthropic's own API, speaking Messages. The key is the console's `x-api-key`, which is the one
/// header every Anthropic-compatible endpoint reads; `ANTHROPIC_AUTH_TOKEN` is deliberately not an
/// alias, because that variable carries an OAuth bearer token for a different header.
///
/// <https://platform.claude.com/docs/en/api/messages>
///
/// The fallback is the model the models overview tells an unsure operator to start with
/// (<https://platform.claude.com/docs/en/about-claude/models/overview>), and the catalogue may
/// carry `anthropic` on a box that has refreshed it - this preset is written to work without one.
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
    effort: None,
};

/// DeepSeek's Anthropic-compatible door: the same models as [`DEEPSEEK`], the Messages wire, and a
/// base URL of `/anthropic` - the endpoint reads `x-api-key` as well as a bearer token, and ignores
/// `anthropic-version`.
///
/// <https://api-docs.deepseek.com/guides/anthropic_api>
///
/// Its catalogue id is `deepseek`'s: the door serves the same models, so a turn is priced by the
/// model's name rather than by the spelling of the URL.
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
};

/// Z.ai's Anthropic-compatible door, which is the one their Claude Code instructions point at:
/// same coding-plan models as [`GLM_CODING`], a different wire.
///
/// <https://docs.z.ai/scenario-example/develop-tools/claude>
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
};

/// Moonshot's Kimi Code plan, whose Claude Code instructions give the base URL
/// `https://api.kimi.ai/coding/` and let the tool append `/v1/messages`. The path here is the whole
/// of it, because this client posts the URL it is given rather than a base URL.
///
/// <https://www.kimi.com/code/docs/en/third-party-tools/claude-code>
///
/// The key variable is not `ANTHROPIC_API_KEY`: a box may hold both doors' keys at once, and one
/// variable for two providers is one provider's key silently sent to the other. The plan and its
/// models are [`KIMI_CODE_GLOBAL`]'s, so it is priced and windowed by that catalogue id.
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
};

/// OpenAI itself, on its own protocol. `gpt-6-astra` is the id the API reference's own
/// examples use, and the fallback for the same reason the others carry one: the catalogue
/// has no `openai` row here, so a box with no operator file has only this to go on.
///
/// ```text
/// POST https://api.openai.com/v1/responses
/// {"model": "gpt-6-astra", "store": false,
///  "include": ["reasoning.encrypted_content"], "stream": true}
/// ```
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
};

/// Google's Gemini, whose wire is nothing like the others: `contents` of `user`/`model` parts,
/// `systemInstruction`, `functionDeclarations`, and the model in the PATH rather than the body, so
/// [`Preset::url`] is a base here rather than an endpoint. [`crate::gemini`] is the client.
///
/// `fallback_model` is the model Google's own list calls the everyday-coding one
/// (<https://ai.google.dev/gemini-api/docs/models>). `catalogue_id` is models.dev's id for the
/// Gemini API; it carries no `google` row in the vendored snapshot yet, so a box with no other
/// catalogue plans against no window and prices nothing — honest, if stingy.
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
};

/// Kimi's coding plan (Moonshot): OpenAI chat completions on the plan's own host. Not the
/// pay-per-token Open Platform (`api.moonshot.ai/v1`, `$MOONSHOT_API_KEY`): the two products'
/// keys and URLs are not interchangeable.
///
/// ```text
/// POST https://api.kimi.com/coding/v1/chat/completions
/// Authorization: Bearer sk-kimi-...
/// {"model":"k3.1-pro", "thinking":{"type":"enabled"}, "reasoning_effort":"high",
///  "prompt_cache_key":"..."}
/// ```
///
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
    &KIMI_CODING,
    &OPENAI,
    &GEMINI,
    &KIMI_CODE,
    &KIMI_CODE_GLOBAL,
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
        cat.model(self.catalogue_id, &model).map(|m| m.context)
    }

    /// The model to use when the operator names none: the catalogue's pick by
    /// rule, else this build's frozen fallback. See [`Preset::fallback_model`].
    pub fn default_model(&self, cat: &crate::catalogue::Catalogue) -> String {
        if self.vendor_pick && cat.model(self.catalogue_id, self.fallback_model).is_some() {
            return self.fallback_model.to_string();
        }
        cat.default_model(self.catalogue_id)
            .unwrap_or_else(|| self.fallback_model.to_string())
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

    /// `--effort` as this door's body field and word, or `None` where it has no switch or no
    /// word for it: the model's own default then stands.
    pub fn effort_for(&self, effort: Option<&str>) -> Option<(&'static str, &'static str)> {
        let e = self.effort?;
        let asked = effort?.trim().to_ascii_lowercase();
        e.words
            .iter()
            .find(|(w, _)| *w == asked)
            .map(|(_, door)| (e.field, *door))
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
            assert_eq!(p.effort.map(|e| e.field), Some("reasoning_effort"));
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

    /// Every `fallback_model` must be a name the catalogue carries in its shortest form,
    /// because a longer alias for the same window and price makes a box with a catalogue
    /// choose differently from one without. The account's own list is what a script sees.
    ///
    /// `openai` is absent on purpose: the vendored snapshot has no `openai` section, so the
    /// check could only pass by inventing a window and a price for a model nobody measured.
    /// Its fallback is the id the API reference's own examples send, which is citable; a
    /// made-up catalogue row would not be.
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
        assert_eq!(KIMI_CODE.effort_for(None), None);
        assert_eq!(DEEPSEEK.effort_for(Some("high")), None);
    }
}
