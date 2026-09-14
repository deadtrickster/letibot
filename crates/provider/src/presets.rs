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
    /// The model used when the operator names none. Chosen as each provider's
    /// general coding model; a reasoning model is a `--model` away.
    pub default_model: &'static str,
    /// Whether the provider wants `reasoning_content` echoed back on assistant
    /// messages of earlier turns. DeepSeek documents that it must NOT be sent
    /// (400 on `deepseek-reasoner` when interleaved with tool calls is the
    /// documented failure); GLM and Grok ignore it. So nobody gets it.
    pub echo_reasoning: bool,
    /// Extra body fields the provider needs to think out loud, if any.
    pub thinking_field: Option<&'static str>,
}

pub const DEEPSEEK: Preset = Preset {
    name: "deepseek",
    url: "https://api.deepseek.com/chat/completions",
    key_env: "DEEPSEEK_API_KEY",
    alt_envs: &[],
    default_model: "deepseek-chat",
    echo_reasoning: false,
    thinking_field: None,
};

pub const GLM: Preset = Preset {
    name: "glm",
    url: "https://open.bigmodel.cn/api/paas/v4/chat/completions",
    key_env: "ZHIPUAI_API_KEY",
    alt_envs: &["ZHIPU_API_KEY", "ZAI_API_KEY", "GLM_API_KEY"],
    default_model: "glm-4.6",
    echo_reasoning: false,
    // Zhipu's `thinking: {"type": "enabled"}` switches GLM's reasoning on; the
    // provider sends it when the operator asks for a reasoning turn.
    thinking_field: Some("thinking"),
};

pub const GROK: Preset = Preset {
    name: "grok",
    url: "https://api.x.ai/v1/chat/completions",
    key_env: "XAI_API_KEY",
    alt_envs: &["GROK_API_KEY"],
    default_model: "grok-4-fast",
    echo_reasoning: false,
    thinking_field: None,
};

pub const ALL: &[&Preset] = &[&DEEPSEEK, &GLM, &GROK];

impl Preset {
    /// `deepseek` | `glm` (`zhipu`, `bigmodel`) | `grok` (`xai`). Anything else
    /// names the three rather than guessing.
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
