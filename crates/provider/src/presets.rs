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
    /// **The fallback default**, for a box with no catalogue.
    ///
    /// Not the default itself: [`Preset::default_model`] asks the catalogue first,
    /// because this constant is exactly the thing that went stale. `deepseek-chat`
    /// and `grok-4-fast` both sat here naming models models.dev had retired, and
    /// `/models deepseek` therefore named something that no longer exists.
    ///
    /// Kept so a box without opencode still has a name to try rather than
    /// refusing, and updated to a model that exists today — but it will go stale
    /// again, and the catalogue is what stops that mattering.
    pub fallback_model: &'static str,
    // **`echo_reasoning` was here, and it is deleted rather than wired.**
    //
    // It was declared once and set three times, and read NOWHERE — so the tree
    // looked like it had considered the question while the running daemon had not.
    // The distinction it named is real, and it belonged to the REQUEST, not to this
    // struct: DeepSeek's thinking-mode guide makes it turn on whether the request
    // carries `tools`, and `OpenAiProvider::body` already knows that. A per-provider
    // boolean could not express it however it was set — the two DeepSeek models
    // disagree with each other, behind one provider name.
    //
    // The old doc comment is worth reading as a lesson rather than as a mistake: it
    // was accurate about `deepseek-reasoner` and wrong about the model in use, and it
    // read like a live switch. See `messages.rs`'s module docs for the measurement
    // that found it and the citation that replaces it.
    /// Extra body fields the provider needs to think out loud, if any.
    pub thinking_field: Option<&'static str>,
    /// **This provider's id in the models.dev catalogue**, which is not always the
    /// name we call it by: we say `glm` and `grok`, the catalogue says `zhipuai`
    /// and `xai`. See [`crate::catalogue`] for why the facts are read from there
    /// rather than written down here.
    pub catalogue_id: &'static str,
}

pub const DEEPSEEK: Preset = Preset {
    name: "deepseek",
    url: "https://api.deepseek.com/chat/completions",
    key_env: "DEEPSEEK_API_KEY",
    alt_envs: &[],
    fallback_model: "deepseek-v4-flash",
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

pub const GROK: Preset = Preset {
    name: "grok",
    url: "https://api.x.ai/v1/chat/completions",
    key_env: "XAI_API_KEY",
    alt_envs: &["GROK_API_KEY"],
    fallback_model: "grok-4.3",
    thinking_field: None,
    catalogue_id: "xai",
};

pub const ALL: &[&Preset] = &[&DEEPSEEK, &GLM, &GROK];

impl Preset {
    /// `deepseek` | `glm` (`zhipu`, `bigmodel`) | `grok` (`xai`). Anything else
    /// names the three rather than guessing.
    /// **The context window to plan compaction against**, from the catalogue.
    ///
    /// `None` when the catalogue has no figure for this model — which happens for
    /// a model it has retired, for a private deployment, and on a box with no
    /// catalogue at all. `None` means the caller leaves the window alone and says
    /// so: the rest of this system is built on *"a window that is not known must
    /// not be invented"*, and inventing one here would decide when a conversation
    /// gets summarised.
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

    pub fn parse(s: &str) -> Result<&'static Preset, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "deepseek" => Ok(&DEEPSEEK),
            "glm" | "zhipu" | "bigmodel" | "z.ai" => Ok(&GLM),
            "grok" | "xai" | "x.ai" => Ok(&GROK),
            other => Err(format!(
                "`{other}` is not a provider this build knows; there are three: deepseek, \
                 glm, grok"
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
        // 1000 prompt of which 600 cached, 100 out:
        // 400*1 + 600*0.1 + 100*2 = 660 per million → 0.00066 USD = 660 micro-USD
        assert_eq!(p.micros(1000, 600, 100), 660);
        assert_eq!(Preset::parse("Zhipu").unwrap().name, "glm");
        assert!(Preset::parse("openai").unwrap_err().contains("three"));
    }
}
