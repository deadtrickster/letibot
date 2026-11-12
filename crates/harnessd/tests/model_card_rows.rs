//! The model card's rows on a box that holds a DeepSeek key: the models that account answers with
//! all reach the card, each with the window and the price the catalogue carries.
//!
//! One test, because `XDG_CONFIG_HOME` is process-global: a sibling writing its own
//! `providers.toml` would decide this file's answer.

use letibot_sessionlog::protocol::{MODEL_FACTS_KEY, MODEL_KEYS_KEY};

#[test]
fn a_key_held_for_a_provider_puts_every_model_it_offers_on_the_card() {
    let scratch = letibot_sessionlog::testing::scratch("letibot-model-card-rows");
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    std::fs::write(
        scratch.join("letibot/providers.toml"),
        "# Written by crates/harnessd/tests/model_card_rows.rs\n[deepseek]\nkey = \"sk-test\"\n",
    )
    .expect("providers.toml");
    // SAFETY: this binary has ONE test, and these are set before any thread that reads config
    // exists - the config below is the first thing built.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODELS_JSON");
    }

    let cfg = letibot_harnessd::config::Config::for_this_box(&scratch);
    let rows = cfg.settings("", false, &[]);
    let model = rows.iter().find(|r| r.key == "model").expect("a model row");
    let deepseek: Vec<&String> = model
        .choices
        .iter()
        .filter(|c| c.starts_with("deepseek/"))
        .collect();
    assert!(
        deepseek.len() > 1,
        "a key is held for deepseek and the card offers one row: {deepseek:?}"
    );
    // The facts travel beside the choices and belong to their own index: a row without its line
    // is a model chosen blind, and a line on the wrong row is worse.
    let facts = rows
        .iter()
        .find(|r| r.key == MODEL_FACTS_KEY)
        .expect("the facts row is published beside the choices");
    assert_eq!(facts.choices.len(), model.choices.len());
    for (name, line) in model.choices.iter().zip(&facts.choices) {
        assert!(!line.is_empty(), "`{name}` has no facts line");
        if name.starts_with("deepseek/") {
            assert!(line.contains("window"), "{name}: {line}");
            assert!(
                line.contains(" in /") || line.contains("unlisted"),
                "no meter for a metered model: {name}: {line}"
            );
        }
    }
    // And the row the head greens with names the provider this block came from.
    let keys = rows
        .iter()
        .find(|r| r.key == MODEL_KEYS_KEY)
        .expect("the keyed-providers row");
    assert!(
        keys.value.split(',').any(|k| k == "deepseek"),
        "the greening disagrees with the listing: {}",
        keys.value
    );
    // A provider this box holds no key for stays one row, whose job is to ask for the key.
    let glm: Vec<&String> = model
        .choices
        .iter()
        .filter(|c| c.starts_with("glm/"))
        .collect();
    assert_eq!(glm.len(), 1, "{glm:?}");
}
