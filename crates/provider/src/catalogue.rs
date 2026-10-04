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
//! # Three places it can come from, and one rule: opencode is read, never depended on
//!
//! 1. **`LETIBOT_MODELS_JSON`** — the operator's explicit pointer, which wins outright.
//! 2. **This harness's own cache**, `~/.cache/letibot/models.json` — a copy somebody put
//!    there on purpose, kept by whatever they keep it with (`scripts/refresh-models-snapshot.sh`
//!    writes it; anything that can `curl` can too).
//! 3. **opencode's cache**, `~/.cache/opencode/models.json` — read when present because
//!    it is fresh and correct, as a courtesy and not as a dependency. The operator,
//!    2026-10-04, on learning the loader read this and nothing else: *"so we depend on
//!    opencode here? not good."* They were right: a box without opencode had NO windows
//!    at all, silently, and every cloud session on it planned compaction against nothing.
//! 4. **The vendored snapshot**, `data/models-snapshot.json` — the five providers a preset
//!    can switch to, fetched the day it was committed ([`SNAPSHOT_FETCHED`]).
//!
//! The first version of this file argued against vendoring — *"a vendored table is a
//! table that goes stale silently"* — and that argument was written when the alternative
//! was three constants and the file belonged to nobody. With the loader reading exactly
//! one other program's cache, the trade flipped: staleness is a defect with a remedy
//! (re-run the script), while absence-with-no-floor is a defect nobody notices until a
//! conversation dies at the provider's real limit. The snapshot's age is not silent
//! either — [`Catalogue::source`] names it with its fetch date wherever the source is
//! disclosed.

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
    source: Option<Origin>,
}

/// Which of the four places a catalogue came from — the fact the disclosure names.
#[derive(Debug)]
enum Origin {
    File(PathBuf),
    /// The vendored snapshot. Carries nothing extra because its date lives beside
    /// it as [`SNAPSHOT_FETCHED`], and the two are regenerated together.
    Snapshot,
}

#[derive(Debug, Default)]
struct RawProviderFacts {
    models: BTreeMap<String, ModelFacts>,
}

/// The operator's explicit pointer, which wins over every other source.
fn override_path() -> Option<PathBuf> {
    std::env::var("LETIBOT_MODELS_JSON").ok().map(PathBuf::from)
}

/// **This harness's own cache** — the first place `load` looks after the override.
///
/// `$XDG_CACHE_HOME/letibot/models.json`, falling back to `~/.cache/letibot/models.json`.
/// Ours, refreshed by `scripts/refresh-models-snapshot.sh` or by anything else that can
/// write a file — a box with no opencode and no operator override still gets a catalogue
/// somebody meant to put there.
pub fn cache_path() -> Option<PathBuf> {
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x),
        _ => Path::new(&std::env::var("HOME").ok()?).join(".cache"),
    };
    Some(base.join("letibot/models.json"))
}

/// Where opencode keeps its copy. **Read as a courtesy, never depended on** — see the
/// module doc for the rule and the report that made it one.
pub fn opencode_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".cache/opencode/models.json"))
}

/// The vendored snapshot's provenance, beside the data it describes. Written by
/// `scripts/refresh-models-snapshot.sh`, which is also what keeps the two honest
/// with each other: the script regenerates the file and prints this string.
pub const SNAPSHOT_FETCHED: &str = "2026-10-04";

/// **The floor**: every provider a preset can switch to, as models.dev carried
/// them on [`SNAPSHOT_FETCHED`]. 5 KB rather than the whole 222-provider file,
/// because this is not a browsing catalogue — it is the answer to *"a box with
/// nothing else still plans against the right window"*, and the providers that
/// matter are the ones `crate::presets` can name.
const SNAPSHOT: &str = include_str!("../data/models-snapshot.json");

/// **A model name's version, and how much is written after it** — the two rungs that ask which
/// model is the newer one, and which name is a plain model rather than a variant of it.
///
/// `glm-5.3` → `([5, 3], 0)`; `glm-5.3-flash` → `([5, 3], 1)`; `glm-5` → `([5], 0)`;
/// `grok-4.20-0309-reasoning` → `([4, 20], 2)`; `deepseek-flash` → `([], 0)`.
///
/// **The version is read from the FIRST segment that carries a digit** — `v4`, `4.20`, `5.3` — and
/// every segment after it counts as written-after, which is what makes a dated snapshot
/// (`-0309-reasoning`), a variant (`-flash`) and a tuned sibling (`-highspeed`) all lose to the
/// plain name, while `glm-5.3` still beats `glm-5.2` on the version rung. A name with no digit in
/// it at all is a plain name with no version to compare.
fn model_version(name: &str) -> (Vec<u64>, usize) {
    let mut version: Vec<u64> = Vec::new();
    let mut written_after = 0usize;
    let mut seen = false;
    for seg in name.split('-') {
        if !seen {
            if seg.chars().any(|c| c.is_ascii_digit()) {
                version = seg
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                seen = true;
            }
            continue;
        }
        written_after += 1;
    }
    (version, written_after)
}

impl Catalogue {
    /// Read the catalogue, or an empty one. **Never an error**: a missing
    /// catalogue is a fact about this box, not a reason for a session to refuse to
    /// open, and every caller already handles `None` for an unknown model.
    ///
    /// The chain is the module doc's: the operator's pointer, then this harness's
    /// own cache, then opencode's, then the snapshot. **Each step falls through on
    /// any failure** — a broken file at the front must not hide a working one
    /// behind it, and the operator's report was about a chain with no floor at all.
    pub fn load() -> Catalogue {
        if let Some(p) = override_path()
            && let Ok(c) = Catalogue::read(&p)
        {
            return c;
        }
        if let Some(p) = cache_path()
            && let Ok(c) = Catalogue::read(&p)
        {
            return c;
        }
        if let Some(p) = opencode_path()
            && let Ok(c) = Catalogue::read(&p)
        {
            return c;
        }
        Catalogue::snapshot()
    }

    /// **The floor, on its own** — what a box with no opencode, no cache and no
    /// override gets. A method rather than an arm of `load` so a test can ask for
    /// exactly the floor, without inventing an environment for `load` to run in.
    pub fn snapshot() -> Catalogue {
        Catalogue::read_text(SNAPSHOT)
            .ok()
            .map(|c| Catalogue {
                source: Some(Origin::Snapshot),
                ..c
            })
            .unwrap_or_default()
    }

    pub fn read(path: &Path) -> Result<Catalogue, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let c = Catalogue::read_text(&text).map_err(|e| format!("{e} ({})", path.display()))?;
        // The origin is the file's; `read_text` names none because a text is not
        // a place, and the snapshot arm is the one caller with no path to name.
        Ok(Catalogue {
            source: Some(Origin::File(path.to_path_buf())),
            ..c
        })
    }

    /// Parse a catalogue's text. The file-naming half of [`Catalogue::read`],
    /// split out so the snapshot — which has no path — goes through the same
    /// parse, the same filters and the same honesty rather than a second reader.
    fn read_text(text: &str) -> Result<Catalogue, String> {
        let raw: BTreeMap<String, RawProvider> =
            serde_json::from_str(text).map_err(|e| format!("parsing the models catalogue: {e}"))?;
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
            source: None,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Which of the four places the catalogue came from, named for the disclosure —
    /// a file by its path, the snapshot by what it is AND the day it was fetched,
    /// because a snapshot's age is the one fact about it that can go stale.
    pub fn source(&self) -> Option<String> {
        self.source.as_ref().map(|o| match o {
            Origin::File(p) => p.display().to_string(),
            Origin::Snapshot => {
                format!("the vendored snapshot (fetched {SNAPSHOT_FETCHED})")
            }
        })
    }

    /// One model's facts, by the catalogue's own provider id.
    pub fn model(&self, provider: &str, model: &str) -> Option<ModelFacts> {
        self.providers.get(provider)?.models.get(model).copied()
    }

    /// Every model name a provider carries, sorted — for a failure message that
    /// can offer the alternatives rather than only naming the mistake.
    pub fn model_names(&self, provider: &str) -> Vec<String> {
        self.providers
            .get(provider)
            .map(|p| p.models.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// **A strictly shorter name for the same window and price, when one exists.**
    ///
    /// This is [`Catalogue::default_model`]'s tiebreak exposed for the one caller that
    /// needs it as a question rather than as a sort: a check on the frozen
    /// `fallback_model`. A provider publishes one model under a rolling alias and under
    /// dated snapshots — `deepseek-flash` beside `deepseek-v4-flash`, same 1M window,
    /// same 0.15 input — and *the alias is the shorter string*. So a fallback for which
    /// this answers `Some` is a longer spelling of a model the catalogue would have
    /// reached under another name, and a box with a catalogue and a box without one
    /// choose differently.
    ///
    /// `None` is the answer to want: nothing shorter names the same thing.
    pub fn shorter_equivalent(&self, provider: &str, name: &str, of: ModelFacts) -> Option<String> {
        self.providers
            .get(provider)?
            .models
            .iter()
            // Same window, same input rate, a different name, and SHORTER than the one
            // being asked about — the shape of a longer alias.
            .filter(|(n, m)| {
                n.as_str() != name
                    && n.len() < name.len()
                    && m.context == of.context
                    && m.prices.map(|p| p.input) == of.prices.map(|p| p.input)
            })
            // The shortest of those, then the name, matching `default_model`'s order so
            // the suggestion is the spelling that rule would have used.
            .min_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(b.0)))
            .map(|(n, _)| n.clone())
    }

    /// The model to use when the operator names none, chosen by rule rather
    /// than written down.
    ///
    /// A hardcoded default is a string that goes stale silently, and both of ours
    /// had: `deepseek-chat` and `grok-4-fast` are models models.dev has retired,
    /// so `/models deepseek` named something that no longer exists. Replacing them
    /// with today's names would buy three months and then be wrong in the same
    /// way, so the constant is the bug and not its value.
    ///
    /// The rule, in order: the largest context window, then the cheapest input
    /// rate, then **the name that writes the least after its version** (a plain model before a
    /// variant or a dated snapshot of it), then **the newest version**, then the SHORTEST name,
    /// then the name itself. That is *"each provider's general coding model"* as
    /// the presets already describe it — the flagship reasoning model is pricier
    /// and stays a `--model` away, and an image or video model loses on context
    /// long before price is reached.
    ///
    /// **The two middle rungs are the operator's, 2026-10-04**, on the coding plan's pick:
    /// *"I want default to be not shortest name lol but the latest model."* They were right about
    /// their own case — five of that plan's models are a 1M window at zero, `glm-5.2` and
    /// `glm-5.3` are both seven characters, and the length rung picked `glm-5.2` on
    /// `"glm-5.2" < "glm-5.3"`. And the qualifier rung comes FIRST so that asking for the
    /// newest is safe: `grok-4.3` sits beside `grok-4.20-0309-reasoning` at the same window and
    /// price, and a bare version comparison would hand every grok session a dated snapshot whose
    /// own name says *reasoning* — and the dated name is the one that gets retired. Measured
    /// across this box's catalogue, the change moves ONE provider's pick (`glm-coding`:
    /// `glm-5.2` → `glm-5.3`) and leaves the other five where they were.
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
                    // **A PLAIN NAME BEATS A DECORATED ONE.** Fewest segments written after the
                    // version: the model itself, or a provider's rolling alias for it, before a
                    // variant (`-flash`, `-reasoning`, `-highspeed`) or a dated snapshot
                    // (`-0309-…`). Without this rung *newest wins* would hand a grok session
                    // `grok-4.20-0309-reasoning` instead of `grok-4.3`, and a deepseek one
                    // `deepseek-v4-flash-vision-exp` instead of the name the account offers.
                    .then_with(|| model_version(&a.0).1.cmp(&model_version(&b.0).1))
                    // **And the newer model wins among the equally plain** — the rung the operator
                    // asked for by name, and the one that moves `glm-5.2` aside for `glm-5.3`.
                    .then_with(|| model_version(&b.0).0.cmp(&model_version(&a.0).0))
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

    /// **The newest model wins — and a PLAIN name still beats a decorated one.**
    ///
    /// The operator, 2026-10-04, on the coding plan's pick: *"I want default to be not shortest
    /// name lol but the latest model."* Their case is the first pair below (`5.2` vs `5.3`, both a
    /// 1M window at zero, both seven characters), and the two other pairs are the guards that keep
    /// the rule safe — written together so neither can be "fixed" into the other:
    ///
    ///   * a **variant of the same version** (`m-5.3-flash`) loses to the plain name;
    ///   * a **dated snapshot whose version sorts higher** (`g-4.20-0309-reasoning` against
    ///     `g-4.3`) also loses — which is the real catalogue's grok shape, and the name that gets
    ///     retired next.
    #[test]
    fn the_default_is_the_newest_model_and_not_a_dated_snapshot_of_it() {
        const FIXTURE: &str = r#"{
          "p": {"id": "p", "models": {
            "m-4.7":       {"limit": {"context": 204800},  "cost": {"input": 0}},
            "m-5.2":       {"limit": {"context": 1000000}, "cost": {"input": 0}},
            "m-5.3":       {"limit": {"context": 1000000}, "cost": {"input": 0}},
            "m-5.3-flash": {"limit": {"context": 1000000}, "cost": {"input": 0}}
          }},
          "g": {"id": "g", "models": {
            "g-4.3":                {"limit": {"context": 1000000}, "cost": {"input": 1.25}},
            "g-4.20-0309-reasoning": {"limit": {"context": 1000000}, "cost": {"input": 1.25}}
          }}
        }"#;
        let cat = read_fixture(FIXTURE);
        // Newer wins where both names are equally plain: `m-5.3`, not `m-5.2` (which the length
        // rung picked on `"m-5.2" < "m-5.3"`) — and not `m-4.7`, which loses on the window first.
        assert_eq!(cat.default_model("p").as_deref(), Some("m-5.3"));
        // A dated snapshot does NOT win for sorting higher: `g-4.20` > `g-4.3` numerically, and the
        // plain alias is what survives the next catalogue refresh.
        assert_eq!(cat.default_model("g").as_deref(), Some("g-4.3"));
        // And the rungs themselves, where they are easier to read than through a pick.
        assert_eq!(model_version("glm-5.3"), (vec![5, 3], 0));
        assert_eq!(model_version("glm-5.3-flash"), (vec![5, 3], 1));
        assert_eq!(model_version("glm-5"), (vec![5], 0));
        assert_eq!(model_version("grok-4.20-0309-reasoning"), (vec![4, 20], 2));
        assert_eq!(model_version("deepseek-flash"), (vec![], 0));
        assert_eq!(model_version("deepseek-v4-flash"), (vec![4], 1));
    }

    /// A catalogue read from a JSON string this test owns, at a path per CALL — these tests run as
    /// parallel threads of one process, and a name keyed on the pid had them deleting each other's
    /// fixture mid-read.
    fn read_fixture(json: &str) -> Catalogue {
        // A counter of this function's OWN: `sample`'s is inside `sample`, and two helpers sharing
        // one would still be two files per call, which is what the name is for.
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "letibot-catalogue-fixture-{}-{}.json",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::write(&d, json).expect("writing");
        let c = Catalogue::read(&d).expect("parsing");
        let _ = std::fs::remove_file(&d);
        c
    }

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

    /// **The floor is real, current, and honest about its age.**
    ///
    /// The operator, 2026-10-04: *"so we depend on opencode here? not good."* The
    /// chain's answer is the snapshot — and a snapshot nobody checks is the stale
    /// table the module's first version argued against. So the test pins the three
    /// things that make a floor honest: it carries every provider a preset can name,
    /// the numbers in it are the ones models.dev published (glm-5.3's 1M is the
    /// number a switched session plans compaction against), and its source names
    /// itself WITH ITS DATE, so the age is on every disclosure the source feeds.
    #[test]
    fn the_snapshot_floor_carries_the_preset_providers_and_names_its_age() {
        let c = Catalogue::snapshot();
        for provider in [
            "deepseek",
            "zhipuai",
            "zai-coding-plan",
            "zhipuai-coding-plan",
            "xai",
        ] {
            assert!(
                !c.model_names(provider).is_empty(),
                "the snapshot has no models for {provider} — a preset names it, so the \
                 floor must carry it (scripts/refresh-models-snapshot.sh)"
            );
        }
        let glm = c
            .model("zai-coding-plan", "glm-5.3")
            .expect("glm-5.3 on the coding plan");
        assert_eq!(
            glm.context, 1_000_000,
            "glm-5.3's window is the number a switched session plans compaction against"
        );
        // **And the floor says what it is, with its date.** A snapshot whose age is
        // silent is the stale table the first version of this file refused to ship.
        let src = c.source().expect("the floor names itself");
        assert!(
            src.contains("vendored snapshot") && src.contains(SNAPSHOT_FETCHED),
            "the source disclosure must name the snapshot AND its fetch date: {src}"
        );
    }

    /// **A file read names the file; a bare text names nothing.** The origin is
    /// what the disclosure draws, so `read` stamping the snapshot's name on a file
    /// (or vice versa) would be a lie with a path in it.
    #[test]
    fn a_file_read_names_the_file_and_a_bare_text_names_nothing() {
        let path = std::env::temp_dir().join(format!(
            "letibot-cat-origin-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(
            &path,
            r#"{"only": {"models": {"m": {"limit": {"context": 1000}}}}}"#,
        )
        .expect("write");
        let c = Catalogue::read(&path).expect("parses");
        let _ = std::fs::remove_file(&path);
        let src = c.source().expect("a file read names the file");
        assert!(src.contains(&path.display().to_string()), "named {src}");
        assert!(!src.contains("snapshot"), "a file is not the floor: {src}");
        assert_eq!(c.model("only", "m").unwrap().context, 1_000);

        // A bare text carries no origin at all: the caller that has one stamps it.
        let bare = Catalogue::read_text(SNAPSHOT).expect("the snapshot parses");
        assert!(bare.source.is_none(), "a text is not a place");
    }

    /// **The fallbacks name real models against the floor itself** — the snapshot's
    /// whole job, asked as the question a box with nothing else asks it.
    #[test]
    fn the_fallback_models_are_real_on_the_floor_alone() {
        let cat = Catalogue::snapshot();
        // The five fallbacks exactly as `crate::presets` spells them — read from the
        // presets themselves rather than re-typed, so a fallback change moves this
        // test with it instead of leaving it to drift into a second list.
        for (preset, provider) in [
            (crate::presets::DEEPSEEK, "deepseek"),
            (crate::presets::GLM, "zhipuai"),
            (crate::presets::GLM_CODING, "zai-coding-plan"),
            (crate::presets::GLM_CODING_CN, "zhipuai-coding-plan"),
            (crate::presets::GROK, "xai"),
        ] {
            assert!(
                cat.model_names(provider)
                    .contains(&preset.fallback_model.to_string()),
                "{}/{} is a preset's fallback and the floor does not carry it",
                provider,
                preset.fallback_model
            );
        }
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
