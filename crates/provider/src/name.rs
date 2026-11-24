//! What a model name means, in one place.
//!
//! The operator types one string for a model on a dozen doors - `--model`, `/models`,
//! `subagent_model`, `leticode_start`, a `task` spawn - and the doors used to disagree: a bare
//! `deepseek` was a provider to one and a local alias to another, so a run that named the door
//! sent the request model literally `deepseek` and the API refused it. One rule here, and the
//! doors call it.

use std::path::Path;

use crate::catalogue::Catalogue;
use crate::presets::Preset;

/// A model string, as exactly one meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Name {
    /// A door this build knows, and the model sent on it. A bare door name resolves to the
    /// door's own default rather than to itself.
    Provider {
        preset: &'static Preset,
        model: String,
    },
    /// A `PROVIDER/MODEL` whose provider resolves to nothing here: kept as the operator spelled
    /// it, so the refusal is the one that lists the doors rather than a guess between meanings.
    Unresolved {
        provider: String,
        model: Option<String>,
    },
    /// A local alias: the daemon's own server, or a `[model."…"]` block's name. Which of the two
    /// is the daemon's to say - this crate does no network and knows no endpoint.
    Local(String),
}

impl Name {
    /// The door, when this names one.
    pub fn preset(&self) -> Option<&'static Preset> {
        match self {
            Name::Provider { preset, .. } => Some(preset),
            _ => None,
        }
    }

    /// The spelling every door stores and prints: `provider/model`, or the alias as it was typed.
    pub fn spec(&self) -> String {
        match self {
            Name::Provider { preset, model } => format!("{}/{model}", preset.name),
            Name::Unresolved { provider, model } => match model {
                Some(m) => format!("{provider}/{m}"),
                None => provider.clone(),
            },
            Name::Local(alias) => alias.clone(),
        }
    }
}

/// [`resolve`]'s answer: what the word means, and the one sentence to say when that is not what
/// it looked like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub name: Name,
    /// `deepseek -> deepseek/deepseek-flash`, or the ambiguity that kept today's meaning. Said
    /// once, by the door that resolved it.
    pub said: Option<String>,
}

/// The model a door sends when the operator names only the door: its own `[<preset>]` table's
/// `model` in providers.toml, else the catalogue's rule, else this build's frozen fallback.
pub fn default_model(preset: &Preset, file: Option<&Path>, cat: &Catalogue) -> String {
    crate::keys::preset_model(preset.name, file).unwrap_or_else(|| preset.default_model(cat))
}

/// `spec` as exactly one meaning.
///
/// `PROVIDER/MODEL` is that pair, unchanged. A bare word a `[model."…"]` block (or `local`)
/// already answers to is the LOCAL alias it is today, because a name the operator's own file
/// declares is theirs to keep; any other bare word [`Preset::parse`] knows is that door and its
/// default model; and anything else is a local alias, today's meaning, so a name nothing here
/// knows is judged by the door it reaches rather than refused by a guess.
pub fn resolve(spec: &str, file: Option<&Path>, cat: &Catalogue) -> Resolved {
    let asked = spec.trim();
    if let Some((provider, model)) = asked.split_once('/') {
        let (provider, model) = (provider.trim(), model.trim());
        if provider.is_empty() {
            return Resolved {
                name: Name::Local(asked.to_string()),
                said: None,
            };
        }
        let Ok(preset) = crate::registry::lookup(provider, file) else {
            // Spelled as a pair, so it stays a pair: the door's own refusal lists the names this
            // build does know, and inventing a local alias here would hide the typo.
            return Resolved {
                name: Name::Unresolved {
                    provider: provider.to_string(),
                    model: (!model.is_empty()).then(|| model.to_string()),
                },
                said: None,
            };
        };
        let model = match model.is_empty() {
            // `deepseek/` names the door and no model: the same answer as the bare word, said the
            // same way so the slash does not quietly mean something else.
            true => default_model(preset, file, cat),
            false => model.to_string(),
        };
        let said = (asked != format!("{}/{model}", preset.name)).then(|| {
            format!(
                "`{asked}` names no model on {0}, so the door's default is sent: `{0}/{model}`",
                preset.name
            )
        });
        return Resolved {
            name: Name::Provider { preset, model },
            said,
        };
    }
    let local = is_local_alias(asked, file);
    match crate::registry::lookup(asked, file) {
        Ok(preset) if !local => {
            let model = default_model(preset, file, cat);
            Resolved {
                said: Some(format!("{asked} -> {}/{model}", preset.name)),
                name: Name::Provider { preset, model },
            }
        }
        // Both a door and a name this box's own file answers to: today's meaning is the local
        // one, and the ambiguity is said rather than resolved silently.
        Ok(preset) if local => Resolved {
            said: Some(format!(
                "`{asked}` is both a door and a local model this box declares; this build keeps \
                 the local model, as it always has. Write `{0}/MODEL` for the door, or \
                 `{0}/{1}` for that door's default",
                preset.name,
                default_model(preset, file, cat)
            )),
            name: Name::Local(asked.to_string()),
        },
        _ => Resolved {
            name: Name::Local(asked.to_string()),
            said: None,
        },
    }
}

/// Whether a bare word is one this box's own `providers.toml` already answers to: `local`, or a
/// `[model."…"]` block with an address - the two names `/models` takes for a model here.
pub fn is_local_alias(word: &str, file: Option<&Path>) -> bool {
    if word == "local" {
        return true;
    }
    crate::keys::local_models(file)
        .iter()
        .any(|m| m.name == word)
}

/// The sentence for a model a door refused that this box's catalogue does not carry: what the
/// door does carry, and the name a bare word for it would have sent. `None` when the catalogue
/// says nothing about the door - an empty list is not evidence about a model.
pub fn unknown_model(
    preset: &Preset,
    model: &str,
    file: Option<&Path>,
    cat: &Catalogue,
) -> Option<String> {
    let carries = cat.model_names(preset.catalogue_id);
    if carries.is_empty() || carries.iter().any(|m| m == model) {
        return None;
    }
    Some(format!(
        "`{}/{model}` is not a model the catalogue carries for {}: it carries {}. A bare `{}` \
         sends {}",
        preset.name,
        preset.name,
        carries.join(", "),
        preset.name,
        default_model(preset, file, cat)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A catalogue carrying one door, scored so the RULE picks `probe-flash`: the widest window
    /// wins before price does, which is why the cheap model is the wide one.
    fn catalogue(tag: &str) -> Catalogue {
        let dir = std::env::temp_dir().join(format!("letibot-name-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.json");
        std::fs::write(
            &path,
            r#"{"probe":{"models":{
                "probe-flash":{"limit":{"context":400000},"cost":{"input":0.1,"output":0.2}},
                "probe-pro":{"limit":{"context":200000},"cost":{"input":2.0,"output":4.0}}}},
              "fam":{"models":{
                "fam-one":{"limit":{"context":128000},"cost":{"input":1.0,"output":1.0}}}}}"#,
        )
        .unwrap();
        Catalogue::read(&path).unwrap()
    }

    fn providers(tag: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("letibot-prov-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The door `probe`, over the catalogue above: a `[provider.probe]` entry, so `catalogue`
    /// names the fixture's provider id.
    fn door(tag: &str, extra: &str) -> std::path::PathBuf {
        providers(
            tag,
            &format!(
                "[provider.probe]\nprotocol = \"chat-completions\"\n\
                 url = \"https://probe.invalid/v1\"\nmodel = \"probe-pro\"\n\
                 catalogue = \"probe\"\nkey = \"sk-t\"\n{extra}"
            ),
        )
    }

    #[test]
    fn a_bare_door_name_is_that_door_and_its_default_model() {
        let cat = catalogue("bare");
        let file = door("bare", "");
        let preset = crate::registry::lookup("probe", Some(&file)).expect("the fixture's door");
        let r = resolve("probe", Some(&file), &cat);
        assert_eq!(
            r.name,
            Name::Provider {
                preset,
                model: "probe-flash".into()
            },
            "the catalogue's rule, not the ask itself"
        );
        assert_eq!(r.said.as_deref(), Some("probe -> probe/probe-flash"));
        assert_eq!(r.name.spec(), "probe/probe-flash");
    }

    #[test]
    fn a_providers_toml_model_line_beats_the_catalogues_rule() {
        let cat = catalogue("line");
        let file = door("line", "\n[probe]\nmodel = \"probe-pro\"\n");
        let r = resolve("probe", Some(&file), &cat);
        assert_eq!(r.name.spec(), "probe/probe-pro", "{r:?}");
        assert_eq!(r.said.as_deref(), Some("probe -> probe/probe-pro"));
        let preset = crate::registry::lookup("probe", Some(&file)).unwrap();
        assert_eq!(
            default_model(preset, Some(&file), &cat),
            "probe-pro",
            "the door's own table is what a bare word sends"
        );
        // Without the line the catalogue's rule answers, and the door's frozen `model` is the
        // last resort - so the two answers are told apart by the line alone.
        let plain = door("line-plain", "");
        let preset = crate::registry::lookup("probe", Some(&plain)).unwrap();
        assert_eq!(default_model(preset, Some(&plain), &cat), "probe-flash");
    }

    #[test]
    fn a_pair_is_unchanged_and_a_slash_with_no_model_takes_the_default() {
        let cat = catalogue("pair");
        let file = door("pair", "");
        let preset = crate::registry::lookup("probe", Some(&file)).unwrap();
        let pair = resolve("probe/probe-pro", Some(&file), &cat);
        assert_eq!(
            pair.name,
            Name::Provider {
                preset,
                model: "probe-pro".into()
            }
        );
        assert_eq!(pair.said, None, "a pair is what it says it is");
        let half = resolve("probe/", Some(&file), &cat);
        assert_eq!(half.name.spec(), "probe/probe-flash");
        assert!(
            half.said
                .as_deref()
                .unwrap_or("")
                .contains("names no model")
        );
    }

    #[test]
    fn a_local_alias_is_unchanged_and_wins_over_a_door_of_the_same_name() {
        let cat = catalogue("local");
        let file = door(
            "local",
            "\n[model.\"probe\"]\nurl = \"http://127.0.0.1:9\"\nmodel = \"local-probe\"\n",
        );
        let r = resolve("probe", Some(&file), &cat);
        assert_eq!(r.name, Name::Local("probe".into()));
        let said = r.said.expect("the ambiguity is said");
        assert!(said.contains("both a door and a local model"), "{said}");
        // And `local` itself stays the daemon's own server, never a door.
        assert_eq!(
            resolve("local", Some(&file), &cat).name,
            Name::Local("local".into())
        );
    }

    #[test]
    fn a_word_that_is_neither_a_door_nor_a_declared_model_keeps_its_old_meaning() {
        let cat = catalogue("other");
        let file = providers("other", "");
        let r = resolve("qwen-3.8-27b", Some(&file), &cat);
        assert_eq!(r.name, Name::Local("qwen-3.8-27b".into()));
        assert_eq!(r.said, None);
        // An unknown provider stays a PAIR, so the door's own refusal lists the names it knows.
        assert!(matches!(
            resolve("nobody/x", Some(&file), &cat).name,
            Name::Unresolved { .. }
        ));
        assert_eq!(
            resolve("nobody/x", Some(&file), &cat).name.spec(),
            "nobody/x",
            "a pair the resolver cannot place is still spelled as it was asked"
        );
    }

    #[test]
    fn a_model_the_door_does_not_carry_names_the_doors_models_and_its_default() {
        let cat = catalogue("unknown");
        let file = door("unknown", "");
        let preset = crate::registry::lookup("probe", Some(&file)).unwrap();
        let said = unknown_model(preset, "probe-huge", Some(&file), &cat).expect("a sentence");
        assert!(
            said.contains("probe-flash") && said.contains("probe-pro"),
            "{said}"
        );
        assert!(said.contains("A bare `probe` sends probe-flash"), "{said}");
        // A model the catalogue does carry is not a refusal, and neither is a door the catalogue
        // has never heard of: an empty list is not evidence.
        assert_eq!(
            unknown_model(preset, "probe-flash", Some(&file), &cat),
            None
        );
        assert_eq!(
            unknown_model(&crate::presets::OPENAI, "anything", Some(&file), &cat),
            None
        );
    }
}
