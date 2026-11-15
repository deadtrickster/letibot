//! A reopened session's list row names the model the resume seated it on, before any turn.

use letibot_harnessd::config::Config;
use letibot_harnessd::{Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{SessionRecord, Store};

/// The registry entry is written from the daemon's default, and only the resume knows the
/// session's own model.
#[test]
fn a_reopened_session_names_its_own_model_before_any_turn() {
    let root = letibot_sessionlog::testing::scratch("lb-reopen-model");
    std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
    // The restore builds the provider, which needs a key: a dummy one, never the operator's.
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        "[deepseek]\nkey = \"sk-test-not-a-key\"\n",
    )
    .expect("providers.toml");
    // SAFETY: this binary has ONE test, and these are set before any thread reads config.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    let db = root.join("sessions.db");
    let mut cfg = Config::for_this_box(&root);
    cfg.store = Some(db.clone());
    cfg.session_id = "s-first".into();
    // The daemon's own default is the local binding, so a stale row says `qwen-3.8-flash-next`.
    cfg.model = "qwen-3.8-flash-next".into();
    cfg.provider = None;
    // Nothing serves this port, and no turn runs here.
    cfg.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 9);
    {
        let store = Store::open(&db).expect("the store opens");
        store
            .put_session(&SessionRecord {
                id: "s-old".into(),
                title: None,
                model_id: "qwen-3.8-flash-next".into(),
                dialect_sha: "00".repeat(32),
                workspace_root: root.display().to_string(),
                owner: "test".into(),
                role: None,
                approvers: vec![],
                parent_session_id: None,
            })
            .expect("the old session's row");
        store
            .set_provider_choice("s-old", Some("deepseek/deepseek-flash"))
            .expect("the previous daemon's switch");
    }
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let registry = Registry::new();
    registry
        .create(&cfg.session_id, "", Sessions::wiring(&cfg))
        .expect("the first session registers");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the first session");
    registry
        .create("s-old", "", Sessions::wiring(&cfg))
        .expect("the reopened session registers");
    assert_eq!(registry.wiring("s-old").model, "qwen-3.8-flash-next");
    sessions.open("s-old").expect("the old session reopens");

    assert_eq!(
        sessions
            .harness_of("s-old")
            .expect("the reopened session")
            .config()
            .prompt_model_name(),
        "deepseek/deepseek-flash",
        "the resume seats the session on its recorded switch"
    );
    let brief = registry.brief("s-old").expect("the row is listed");
    assert_eq!(
        brief.wiring.model, "deepseek/deepseek-flash",
        "the list row names the daemon's default until a turn runs"
    );
    registry.close();
    drop(sessions);
    let _ = std::fs::remove_dir_all(&root);
}
