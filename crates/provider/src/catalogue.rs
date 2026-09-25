//! **The model catalogue, read rather than remembered.**
//!
//! Compaction is planned against one number: `Config::context_window`, which
//! `plan_overrun`, `should_compact` and `headroom` all read. It comes from the
//! local server's `/props`, and switching a session to a metered provider left it
//! there — so a conversation answered by a cloud model went on being measured
//! against the llama-server's window, and compacted at the wrong time or not at
//! all.
//!
//! The obvious fix was three constants in [`crate::presets`]. They were written,
//! and every one of them was wrong: deepseek's default is a million tokens and not
//! 64k, `glm-4.6` is not in the current catalogue at all, grok is a million too.
//! The operator, before any of it shipped: *"look how opencode handles it, we can
//! steal config pieces from it"*.
//!
//! opencode keeps the [models.dev] catalogue at `~/.cache/opencode/models.json`
//! and refreshes it. 222 providers, every model, with `limit.context`,
//! `limit.output` and `cost` per model. So this reads that file when it is there
//! and answers `None` when it is not — which is the honest answer and the one the
//! rest of the code is built for: *"a window that is not known must not be
//! invented"*.
//!
//! [models.dev]: https://models.dev
//!
//! # Why not vendor a copy
//!
//! A vendored table is a table that goes stale silently, and a stale context
//! window is not a cosmetic error: it decides when a conversation is summarised.
//! Reading a file somebody else keeps fresh has a real failure mode — it can be
//! absent — and that failure is loud and recoverable, which the stale one is not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One model's published limits and prices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelFacts {
    /// Total context, in tokens. The number compaction is planned against.
    pub context: u64,
    /// The most it will generate in one turn.
    pub output: u64,
    /// USD per million, as [`crate::presets::Prices`] wants them. `None` for a
    /// model the catalogue prices nowhere — some are free, some are unlisted, and
    /// those are different from zero.
    pub prices: Option<crate::presets::Prices>,
}

#[derive(Debug, Deserialize)]
struct RawProvider {
    #[serde(default)]
    models: BTreeMap<String, RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    #[serde(default)]
    limit: Option<RawLimit>,
    #[serde(default)]
    cost: Option<RawCost>,
}

#[derive(Debug, Deserialize)]
struct RawLimit {
    #[serde(default)]
    context: u64,
    #[serde(default)]
    output: u64,
}

#[derive(Debug, Deserialize)]
struct RawCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    /// models.dev's name for the cached-read rate. Absent means the provider does
    /// not discount a cache hit, so it costs the full input rate — not zero.
    #[serde(default)]
    cache_read: Option<f64>,
}

/// The catalogue, parsed once.
#[derive(Debug, Default)]
pub struct Catalogue {
    providers: BTreeMap<String, RawProviderFacts>,
    /// Where it was read from, for the disclosure.
    source: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct RawProviderFacts {
    models: BTreeMap<String, ModelFacts>,
}

/// Where opencode keeps it. Overridable, because a box without opencode can point
/// at its own copy rather than doing without.
pub fn default_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LETIBOT_MODELS_JSON") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".cache/opencode/models.json"))
}

impl Catalogue {
    /// Read the catalogue, or an empty one. **Never an error**: a missing
    /// catalogue is a fact about this box, not a reason for a session to refuse to
    /// open, and every caller already handles `None` for an unknown model.
    pub fn load() -> Catalogue {
        match default_path() {
            Some(p) => Catalogue::read(&p).unwrap_or_default(),
            None => Catalogue::default(),
        }
    }

    pub fn read(path: &Path) -> Result<Catalogue, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let raw: BTreeMap<String, RawProvider> =
            serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        let mut providers = BTreeMap::new();
        for (id, p) in raw {
            let mut models = BTreeMap::new();
            for (mid, m) in p.models {
                // A model with no published limit is not carried: the whole point
                // is to stop inventing this number, and a zero would invent the
                // smallest possible one.
                let Some(l) = m.limit.filter(|l| l.context > 0) else {
                    continue;
                };
                models.insert(
                    mid,
                    ModelFacts {
                        context: l.context,
                        output: l.output,
                        prices: m.cost.map(|c| crate::presets::Prices {
                            input: c.input,
                            // No `cache_read` means no discount, which is the input
                            // rate. Defaulting it to zero would report a metered
                            // turn as free.
                            cached: c.cache_read.unwrap_or(c.input),
                            output: c.output,
                        }),
                    },
                );
            }
            providers.insert(id, RawProviderFacts { models });
        }
        Ok(Catalogue {
            providers,
            source: Some(path.to_path_buf()),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// One model's facts, by the catalogue's own provider id.
    pub fn model(&self, provider: &str, model: &str) -> Option<ModelFacts> {
        self.providers.get(provider)?.models.get(model).copied()
    }

    /// **The model to use when the operator names none**, chosen by rule rather
    /// than written down.
    ///
    /// A hardcoded default is a string that goes stale silently, and both of ours
    /// had: `deepseek-chat` and `grok-4-fast` are models models.dev has retired,
    /// so `/models deepseek` named something that no longer exists. Replacing them
    /// with today's names would buy three months and then be wrong in the same
    /// way, so the constant is the bug and not its value.
    ///
    /// The rule, in order: the largest context window, then the cheapest input
    /// rate, then the SHORTEST name, then the name itself. That is *"each
    /// provider's general coding model"* as
    /// the presets already describe it — the flagship reasoning model is pricier
    /// and stays a `--model` away, and an image or video model loses on context
    /// long before price is reached.
    ///
    /// A model with no price is skipped: unpriced and free are different, and a
    /// default that silently picked an unpriced model would report every metered
    /// turn as costing nothing.
    pub fn default_model(&self, provider: &str) -> Option<String> {
        self.providers
            .get(provider)?
            .models
            .iter()
            .filter(|(_, m)| m.prices.is_some())
            .min_by(|a, b| {
                b.1.context
                    .cmp(&a.1.context)
                    .then_with(|| {
                        let (x, y) = (
                            a.1.prices.map(|p| p.input).unwrap_or(f64::MAX),
                            b.1.prices.map(|p| p.input).unwrap_or(f64::MAX),
                        );
                        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    // **Shortest name wins a tie.** A provider publishes the same
                    // model under a rolling alias and under dated snapshots —
                    // `grok-4.3` beside `grok-4.20-0309-non-reasoning`, identical
                    // window and price — and ordering by name alone picked the
                    // snapshot, which is the one that gets retired. The alias is
                    // always the shorter string.
                    .then_with(|| a.0.len().cmp(&b.0.len()))
                    .then_with(|| a.0.cmp(b.0))
            })
            .map(|(name, _)| name.clone())
    }

    /// Every model a provider lists that carries a context limit, largest first —
    /// for a picker, and for a miss report that names what it could have been.
    pub fn models(&self, provider: &str) -> Vec<(String, ModelFacts)> {
        let Some(p) = self.providers.get(provider) else {
            return Vec::new();
        };
        let mut out: Vec<(String, ModelFacts)> =
            p.models.iter().map(|(k, v)| (k.clone(), *v)).collect();
        out.sort_by(|a, b| b.1.context.cmp(&a.1.context).then(a.0.cmp(&b.0)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "deepseek": {
        "id": "deepseek",
        "models": {
          "deepseek-chat": {"limit": {"context": 1000000, "output": 384000},
                            "cost": {"input": 0.15, "output": 0.6, "cache_read": 0.003}},
          "deepseek-legacy": {"limit": {"context": 64000, "output": 8000}},
          "an-image-model": {"limit": {"context": 0, "output": 0}}
        }
      },
      "zhipuai": {
        "id": "zhipuai",
        "models": {
          "glm-5": {"limit": {"context": 204800, "output": 131072},
                    "cost": {"input": 1.0, "output": 3.2}}
        }
      }
    }"#;

    /// A file per CALL, not per process: these tests run as parallel threads of
    /// one process, and a name keyed on the pid had them deleting each other's
    /// fixture mid-read.
    fn sample() -> Catalogue {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "letibot-cat-{}-{}.json",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::write(&d, SAMPLE).expect("writing");
        let c = Catalogue::read(&d).expect("parsing");
        let _ = std::fs::remove_file(&d);
        c
    }

    #[test]
    fn a_models_context_and_prices_come_from_the_catalogue() {
        let c = sample();
        let m = c.model("deepseek", "deepseek-chat").expect("the model");
        assert_eq!(m.context, 1_000_000);
        assert_eq!(m.output, 384_000);
        let p = m.prices.expect("priced");
        assert_eq!(p.input, 0.15);
        assert_eq!(p.cached, 0.003);
    }

    /// **A model with no cache discount is charged at the input rate**, not at
    /// zero. Defaulting the missing field would report a metered turn as free.
    #[test]
    fn a_model_with_no_cache_rate_is_not_free_to_re_read() {
        let p = sample()
            .model("zhipuai", "glm-5")
            .expect("the model")
            .prices
            .expect("priced");
        assert_eq!(p.cached, p.input, "no discount is the input rate");
    }

    /// A model the catalogue does not carry, and a model whose limit is zero, are
    /// both `None` — the whole point is to stop inventing this number.
    #[test]
    fn an_unknown_model_is_none_rather_than_a_guess() {
        let c = sample();
        assert!(c.model("deepseek", "deepseek-reasoner").is_none());
        assert!(c.model("nobody", "nothing").is_none());
        assert!(
            c.model("deepseek", "an-image-model").is_none(),
            "a zero limit is not a limit"
        );
    }

    /// The default is derived, so it cannot go stale the way two hardcoded ones
    /// already had. Largest window, then cheapest, then the name.
    #[test]
    fn the_default_model_is_the_biggest_window_at_the_lowest_price() {
        let c = sample();
        assert_eq!(
            c.default_model("deepseek").as_deref(),
            Some("deepseek-chat")
        );
        // `deepseek-legacy` has no price, so it is skipped rather than chosen —
        // unpriced and free are different, and a default that picked an unpriced
        // model would report every metered turn as costing nothing.
        assert_eq!(c.default_model("zhipuai").as_deref(), Some("glm-5"));
        assert!(c.default_model("nobody").is_none());
    }

    /// **A rolling alias beats a dated snapshot of the same model.** Providers
    /// publish both at identical window and price, and ordering by name alone
    /// picked the snapshot — which is the one that gets retired, which is how the
    /// hardcoded defaults went stale in the first place.
    #[test]
    fn a_tie_goes_to_the_shorter_name() {
        let d = std::env::temp_dir().join(format!(
            "letibot-cat-tie-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(
            &d,
            r#"{"xai":{"id":"xai","models":{
                 "grok-4.20-0309-non-reasoning": {"limit":{"context":1000000,"output":30000},
                                                  "cost":{"input":1.25,"output":2.5}},
                 "grok-4.3": {"limit":{"context":1000000,"output":30000},
                              "cost":{"input":1.25,"output":2.5}}}}}"#,
        )
        .expect("writing");
        let c = Catalogue::read(&d).expect("parsing");
        let _ = std::fs::remove_file(&d);
        assert_eq!(c.default_model("xai").as_deref(), Some("grok-4.3"));
    }

    #[test]
    fn a_providers_models_come_back_largest_window_first() {
        let names: Vec<String> = sample()
            .models("deepseek")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["deepseek-chat", "deepseek-legacy"]);
    }

    /// **The meter reads the catalogue when the operator's file is silent.**
    /// `providers.toml` priced `deepseek-chat`, a model DeepSeek has retired, so
    /// every turn on `deepseek-flash` reported `cost unpriced` — the operator,
    /// watching a metered conversation: *"also no money meter"*. The file still
    /// wins where it has an entry: that is their own number and may be a contract
    /// price.
    #[test]
    fn a_model_the_operators_file_does_not_price_is_priced_by_the_catalogue() {
        let c = sample();
        let p = c
            .model("deepseek", "deepseek-chat")
            .expect("the model")
            .prices
            .expect("the catalogue prices it");
        // 1000 prompt of which 600 cached, 100 out, at 0.15 / 0.003 / 0.6 USD
        // per million: 400*0.15 + 600*0.003 + 100*0.6 = 60 + 1.8 + 60 = 121.8,
        // and the unit is micro-USD.
        assert_eq!(p.micros(1000, 600, 100), 122);
        // And a model nothing prices stays unpriced, because unpriced and free
        // are different.
        assert!(
            c.model("deepseek", "deepseek-legacy")
                .expect("the model")
                .prices
                .is_none()
        );
    }

    /// A box with no opencode is a box with no catalogue, and that must not be an
    /// error — every caller already handles the unknown case.
    #[test]
    fn a_missing_catalogue_is_empty_rather_than_a_failure() {
        let c = Catalogue::read(Path::new("/nonexistent/models.json"));
        assert!(c.is_err(), "read reports it");
        assert!(Catalogue::default().is_empty());
        assert!(
            Catalogue::default()
                .model("deepseek", "deepseek-chat")
                .is_none()
        );
    }
}
