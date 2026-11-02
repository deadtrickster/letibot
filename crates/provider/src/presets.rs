//! The three providers, as facts about their doors.

/// One provider's shape: where it is, which header carries the key, and which
/// environment variable the operator will have put the key in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub url: &'static str,
    pub key_env: &'static str,
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
}

pub const DEEPSEEK: Preset = Preset {
    name: "deepseek",
    url: "https://api.deepseek.com/chat/completions",
    key_env: "DEEPSEEK_API_KEY",
    alt_envs: &[],
    fallback_model: "deepseek-flash",
    thinking_field: None,
    catalogue_id: "deepseek",
};

pub const GLM: Preset = Preset {
    name: "glm",
    url: "https://open.bigmodel.cn/api/paas/v4/chat/completions",
    key_env: "ZHIPUAI_API_KEY",
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    fallback_model: "glm-5.3-flash",
    // Zhipu's `thinking: {"type": "enabled"}` switches GLM's reasoning on; the
    // provider sends it when the operator asks for a reasoning turn.
    thinking_field: Some("thinking"),
    catalogue_id: "zhipuai",
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
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    // The catalogue's rule picks this: the plan's models tie on window and price, so the
    // qualifier and version rungs decide, and they name glm-5.3.
    fallback_model: "glm-5.3",
    thinking_field: Some("thinking"),
    catalogue_id: "zai-coding-plan",
};

/// Zhipu's coding plan: the mainland host of the same product as [`GLM_CODING`].
pub const GLM_CODING_CN: Preset = Preset {
    name: "glm-coding-cn",
    url: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
    key_env: "ZHIPUAI_API_KEY",
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    fallback_model: "glm-5.3",
    thinking_field: Some("thinking"),
    catalogue_id: "zhipuai-coding-plan",
};

pub const GROK: Preset = Preset {
    name: "grok",
    url: "https://api.x.ai/v1/chat/completions",
    key_env: "XAI_API_KEY",
    alt_envs: &["GROK_API_KEY"],
    fallback_model: "grok-4.3",
    thinking_field: None,
    catalogue_id: "xai",
};

pub const ALL: &[&Preset] = &[&DEEPSEEK, &GLM, &GLM_CODING, &GLM_CODING_CN, &GROK];

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
        cat.default_model(self.catalogue_id)
            .unwrap_or_else(|| self.fallback_model.to_string())
    }

    /// Names this build knows, with the aliases each answers to; anything else is an
    /// error naming all five rather than a guess.
    pub fn parse(s: &str) -> Result<&'static Preset, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "deepseek" => Ok(&DEEPSEEK),
            "glm" | "zhipu" | "bigmodel" | "z.ai" => Ok(&GLM),
            // Named separately because reaching for the subscription under the metered
            // name answers a 429 that reads like an empty account. See [`GLM_CODING`].
            "glm-coding" | "zai-coding" | "coding-plan" => Ok(&GLM_CODING),
            "glm-coding-cn" | "zhipu-coding" | "bigmodel-coding" => Ok(&GLM_CODING_CN),
            "grok" | "xai" | "x.ai" => Ok(&GROK),
            other => Err(format!(
                "`{other}` is not a provider this build knows; there are five: deepseek, \
                 glm, glm-coding, glm-coding-cn, grok"
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
        assert!(Preset::parse("openai").unwrap_err().contains("five"));
    }

    /// Every `fallback_model` must be a name the catalogue carries in its shortest form,
    /// because a longer alias for the same window and price makes a box with a catalogue
    /// choose differently from one without. The account's own list is what a script sees.
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
}
