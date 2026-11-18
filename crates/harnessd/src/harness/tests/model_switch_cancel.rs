//! A switch made while a turn runs waits for the turn's end; choosing the model already on calls
//! it off.

use super::*;

#[test]
fn choosing_the_current_model_calls_off_a_waiting_switch() {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("switch-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let file = dir.join("providers.toml");
    std::fs::write(
        &file,
        "[deepseek]\nkey = \"k\"\nurl = \"http://127.0.0.1:9/v1/chat/completions\"\n\n\
         [glm]\nkey = \"k\"\nurl = \"http://127.0.0.1:9/v1/chat/completions\"\n",
    )
    .unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.session_id = "s-switch-cancel".into();
    cfg.providers_file = Some(file);
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no file");
    let mut h = Harness::open(&parts, cfg, Hub::new("s-switch-cancel")).expect("the session opens");
    let on = |name: &str, model: &str| crate::config::ProviderConfig {
        name: name.into(),
        model: Some(model.into()),
        api_key: None,
        thinking: false,
    };
    h.set_provider(Some(on("deepseek", "deepseek-flash")))
        .expect("the first model");

    // A turn is in flight: a switch to another model waits for its end.
    h.prompt_in_flight.store(true, Ordering::SeqCst);
    let said = h
        .set_provider(Some(on("glm", "glm-4.6")))
        .expect("the switch is taken");
    assert!(said.contains("when this turn ends"), "{said}");
    assert!(h.pending_switch.is_some(), "it waits");

    // Choosing the model already on calls it off, and says so.
    let said = h
        .set_provider(Some(on("deepseek", "deepseek-flash")))
        .expect("the call-off is taken");
    assert!(said.contains("called off"), "{said}");
    assert!(
        h.pending_switch.is_none(),
        "the switch would still land when the turn ends"
    );
    h.prompt_in_flight.store(false, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(&dir);
}
