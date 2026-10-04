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
    /// refusing — but it will go stale again, and the catalogue is what stops that
    /// mattering.
    ///
    /// # It must be a name the account OFFERS, and being *accepted* is not that
    ///
    /// MEASURED 2026-10-02 against the live DeepSeek account, because the value here
    /// had drifted to `deepseek-v4-flash` — which the catalogue's own pick-by-rule
    /// deliberately avoids:
    ///
    /// ```text
    /// GET /models            -> deepseek-flash, deepseek-v4-pro        (offered: 2)
    /// POST /chat/completions -> accepts deepseek-flash, deepseek-v4-flash,
    ///                           deepseek-v4-pro, deepseek-chat
    ///                           and answers with `model: deepseek-flash` for the two
    ///                           names it does not offer                  (accepted: 4)
    /// ```
    ///
    /// So a wrong name here does **not** fail loudly — it is silently aliased onto a
    /// model that does exist, and the operator never sees an error. That is the whole
    /// hazard: an alias is a **deprecation path**, so a fallback that only works
    /// because of one is a fallback with a lifetime nobody has written down, on the
    /// path taken when something else has already gone wrong.
    ///
    /// It also contradicted this tree's own rule. [`Catalogue::default_model`] orders
    /// by biggest window, then lowest price, then **shortest name** — and its comment
    /// says why: *"A provider publishes the same model under a rolling alias and
    /// under dated snapshots … The alias is always the shorter string."* The
    /// catalogue therefore picks `deepseek-flash` while this froze the longer
    /// `deepseek-v4-flash`, so a box WITH a catalogue and a box WITHOUT one chose
    /// different models.
    ///
    /// The check that measures it is `scripts/check-provider-models.sh`, which asks
    /// each provider's own `/models` and refuses a fallback that is not offered. It
    /// skips (loudly) where no key resolves, because that is a fact about the box.
    ///
    /// [`Catalogue::default_model`]: crate::catalogue::Catalogue::default_model
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

/// **The subscriptions, not the meter — and opencode's own box is where the difference was read.**
///
/// MEASURED 2026-10-04. The operator put a session on `glm/glm-5.3` and the endpoint answered
///
/// ```text
/// http 429: 余额不足或无可用资源包,请充值。
/// ```
///
/// — *insufficient balance or no available resource package, please top up* — for an account that
/// is **not** out of money: their key is for a **coding plan**, and [`GLM`] sends every GLM turn to
/// the raw pay-as-you-go API at `open.bigmodel.cn/api/paas/v4`, which sees a key with no balance
/// behind it.
///
/// **The reference distinguishes them, and so does the catalogue.** opencode's `auth.json` on this
/// box files the key under `zai-coding-plan` (`type: api`), its log says
/// `providerID=zai-coding-plan modelID=glm-5.3`, and models.dev carries four entries rather than
/// one product:
///
/// ```text
/// zai                  https://api.z.ai/api/paas/v4              18 models
/// zai-coding-plan      https://api.z.ai/api/coding/paas/v4        7 models
/// zhipuai              https://open.bigmodel.cn/api/paas/v4      17 models
/// zhipuai-coding-plan  https://open.bigmodel.cn/api/coding/paas/v4 4 models
/// ```
///
/// A plan is a different product on the same host — `/api/coding/paas/v4` rather than
/// `/api/paas/v4` — with **its own model list** (the plan serves `glm-5.3`, `glm-5.3-flash`,
/// `glm-5.3-highspeed` and a few more, and nothing else), its own billing (the plan's models are
/// priced at **zero**, because the subscription has already paid for them — a metered reading would
/// invent a bill nobody is charged) and the same key variable, because what a key can reach is a
/// fact about the account and not about a second secret.
///
/// **Two presets and not a base-URL override**, because all three of those facts live on the
/// catalogue row, and a free-form URL would leave the window, the picker and the derived default
/// pointing at the raw product. `glm-coding` is Z.AI's (global — what this box's key is for) and
/// `glm-coding-cn` is Zhipu's (mainland): same product, different host, different model list.
///
/// **The money meter stays quiet on these**, which is honest rather than broken: nothing here is
/// metered per token.
pub const GLM_CODING: Preset = Preset {
    name: "glm-coding",
    url: "https://api.z.ai/api/coding/paas/v4/chat/completions",
    key_env: "ZHIPUAI_API_KEY",
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    // **`glm-5.3`, which is what the catalogue's rule picks — and it took a rule change to get
    // here.** Five of this plan's models are a 1M window at a zero price, so the window and the
    // price leave all five; the qualifier rung then drops `-highspeed` and `-flash`, and the
    // version rung picks `glm-5.3` over `glm-5.2`. Before that rung existed the length tie-break
    // picked `glm-5.2` on `"glm-5.2" < "glm-5.3"` — the answer the operator rejected by name.
    fallback_model: "glm-5.3",
    thinking_field: Some("thinking"),
    catalogue_id: "zai-coding-plan",
};

/// Zhipu's coding plan — the mainland host of the same product as [`GLM_CODING`].
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
    /// `deepseek` | `glm` (`zhipu`, `bigmodel`) | `glm-coding` (`zai-coding`) |
    /// `glm-coding-cn` | `grok` (`xai`). Anything else names them rather than guessing.
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
            // **The subscription is named separately and must be**, because the failure of
            // reaching for it with the other name is a 429 that reads like an empty account:
            // see [`GLM_CODING`].
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
        // 1000 prompt of which 600 cached, 100 out:
        // 400*1 + 600*0.1 + 100*2 = 660 per million → 0.00066 USD = 660 micro-USD
        assert_eq!(p.micros(1000, 600, 100), 660);
        assert_eq!(Preset::parse("Zhipu").unwrap().name, "glm");
        assert!(Preset::parse("openai").unwrap_err().contains("five"));
    }

    /// **Every `fallback_model` must be a name the catalogue also carries, and must be
    /// the SHORTEST name for what it names.**
    ///
    /// Both halves are here because the two failures are different, and only one of
    /// them is visible from inside the tree:
    ///
    /// * **Invented or retired** — the name is not in the catalogue at all. This is the
    ///   failure the doc comment on the field records: `deepseek-chat` and
    ///   `grok-4-fast` sat here after models.dev had retired them. Caught by the first
    ///   assertion.
    /// * **The long alias** — the name IS in the catalogue, and so is the model under a
    ///   shorter one, at the same window and the same price. MEASURED 2026-10-02:
    ///   `deepseek-v4-flash` was here while `deepseek-flash` — the name the account
    ///   actually offers, and the one [`Catalogue::default_model`] picks — sat beside
    ///   it with identical figures. A box with a catalogue and a box without one
    ///   therefore chose different models, and the live API aliased the wrong name
    ///   onto the right model so nothing ever failed. Caught by the second assertion,
    ///   which applies the same rule the catalogue does for the same reason.
    ///
    /// Skipped — loudly — where the catalogue is not readable, because the absence is
    /// a fact about the box rather than about this constant. **What no test here can
    /// see is the account's own model list**, which is the authority: models.dev
    /// carried `deepseek-v4-flash` while the account did not offer it. That is what
    /// `scripts/check-provider-models.sh` is for, and it needs a key.
    ///
    /// [`Catalogue::default_model`]: crate::catalogue::Catalogue::default_model
    #[test]
    fn every_fallback_names_a_real_model_and_its_shortest_form() {
        let cat = crate::catalogue::Catalogue::load();
        if cat.is_empty() {
            eprintln!(
                "SKIPPED: no catalogue at {}, so the fallback names cannot be checked \
                 against it — run scripts/check-provider-models.sh with a key for the \
                 check that matters",
                crate::catalogue::Catalogue::load()
                    .source()
                    .unwrap_or_else(|| "(no source at all)".into())
            );
            return;
        }
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
            // **No SHORTER name may name the same window and price.** If one does, the
            // fallback is a longer spelling of a model the catalogue's own rule would
            // have reached by the shorter one — so a box with a catalogue and a box
            // without one choose differently, and the live API may alias the longer
            // name onto the shorter model silently, which is how this went unnoticed.
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
