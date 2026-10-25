//! **A session says who ANSWERS, not what the vocabulary binds** — the operator's *"with
//! `--new` it started with qwen, not default model"*, 2026-10-26.
//!
//! Two models are in play in every daemon and they are different facts:
//!
//! * `Config::model` — the local **binding**, the alias whose GGUF renders the prompt and whose
//!   ids the ledger counts. The launcher passes it on every start (`--model`), so it is
//!   `qwen-3.8-flash-next` even on a box whose turns never go to a local server.
//! * `Config::prompt_model_name()` — **who answers**, which is `deepseek/deepseek-flash` when
//!   `~/.config/letibot/providers.toml` says `[default] provider = "deepseek"`.
//!
//! `Sessions::wiring` (what a head's `Hello` carries and what the picker draws) and the
//! `model_id` a session's store row keeps were both written from the BINDING. So a session the
//! operator started on their default provider was described as a local one in three places at
//! once — the header's first frame, the ctrl-s picker, `letibot --sessions` — while its turns
//! went to deepseek. The `model` settings row was already right, which is what made it visible:
//! one session, two names.
//!
//! # What this asserts, and what it deliberately does not
//!
//! It asserts the DESCRIPTION: the wiring and the row name the answering model, and a session
//! opened later by a head (`Work::Open`, the door `letibot --new` reaches) is described the same
//! way as the daemon's first one. It does **not** assert that the turns go to deepseek — that is
//! `Config::provider`, which `harnessd`'s own startup resolves once and `Sessions` clones into
//! every session it opens; re-measuring it here would need a provider, a key and a network.
//!
//! No GGUF and no model: the byte vocabulary is the door `idle_clock.rs` and `no_vocab.rs` take,
//! so this runs on a machine with no vocabulary file.

use std::path::PathBuf;

use letibot_harnessd::config::{Config, ModelSource, ProviderConfig};
use letibot_harnessd::leticode_config::Level;
use letibot_harnessd::{Parts, Sessions};
use letibot_sessionlog::registry::Registry;

/// A directory that removes itself, so a leaked store per run does not fill the disk.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("the temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The box the operator described: `[default] provider = "deepseek"` in `providers.toml`, and a
/// launcher that passed the local binding on the command line.
fn the_operators_box(dir: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box(dir);
    cfg.store = Some(dir.join("sessions.db"));
    cfg.session_id = "s-first".into();
    // What the launcher passes: the vocabulary this box has a GGUF for.
    cfg.model = "qwen-3.8-flash-next".into();
    // Nothing serves this port — no turn runs here, and a test must not be able to reach the
    // box's own model server by accident.
    cfg.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 9);
    // `[default] provider = "deepseek"`, resolved by `harnessd`'s startup before any session.
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-flash".into()),
        api_key: None,
        thinking: false,
    });
    cfg.model_source = ModelSource {
        level: Level::Builtin,
        value: Some("deepseek/deepseek-flash".into()),
        file: None,
        origin: "the built-in level, in this test's own words".into(),
        note: None,
    };
    cfg
}

/// **The wiring a head is handed names the answering model, not the bound one.**
///
/// `SessionWiring` is the `Hello`'s word about the session, and the header draws it before any
/// settings row has arrived — which is exactly the first frame of a session that was just
/// created, so this is the frame the operator read as *"it started with qwen"*.
#[test]
fn a_sessions_wiring_names_who_answers_and_not_the_vocabulary() {
    let dir = TempDir::new("lb-wiring-model");
    let cfg = the_operators_box(&dir.path);

    let wiring = Sessions::wiring(&cfg);
    assert_eq!(
        wiring.model, "deepseek/deepseek-flash",
        "the wiring says `qwen-3.8-flash-next` — the local BINDING — about a session whose turns \
         go to the operator's `[default]` provider, and that string is what the header draws \
         before the first settings row arrives"
    );
    // The other two fields were never the binding and stay as they were: an endpoint is where
    // the vocabulary's server would be, and it is reported, not chosen.
    assert_eq!(wiring.dialect, cfg.dialect.name());
    assert_eq!(wiring.endpoint, cfg.endpoint.authority());
}

/// **And a session opened LATER — a head's `--new`, `Work::Open` — is described the same way.**
///
/// The lazy door (`Sessions::harness_as`) clones the daemon's config, so the session runs on the
/// daemon's provider; what this pins is that nothing about that door picks the binding up again
/// for the description. The row it writes and the wiring it registers are the two things a
/// picker and a `Hello` read.
#[test]
fn a_session_a_head_opens_is_described_as_the_first_one_was() {
    let dir = TempDir::new("lb-new-session-model");
    let cfg = the_operators_box(&dir.path);
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let registry = Registry::new();
    registry
        .create(&cfg.session_id, "", Sessions::wiring(&cfg))
        .expect("the first session registers");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the first session");

    registry
        .create("s-new", "", Sessions::wiring(&cfg))
        .expect("the head's session registers");
    sessions.open("s-new").expect("the head's session opens");

    // The answers are the same model, which is the fact underneath both descriptions.
    let first = sessions.harness_of("s-first").expect("the first session");
    let new = sessions.harness_of("s-new").expect("the created session");
    assert_eq!(
        first.config().prompt_model_name(),
        "deepseek/deepseek-flash",
        "the first session answers on the operator's default provider"
    );
    assert_eq!(
        new.config().prompt_model_name(),
        first.config().prompt_model_name(),
        "the session `--new` opened answers on a different model from the one the daemon opened \
         with — the operator's report, and a daemon has one model for the sessions it opens"
    );

    // And its row, which is what `letibot --sessions` and the picker read after a restart.
    let store = letibot_tokencore::store::Store::open(&dir.path.join("sessions.db"))
        .expect("the store opens");
    let row = store
        .session("s-new")
        .expect("the store answers")
        .expect("the created session has a row");
    assert_eq!(
        row.model_id, "deepseek/deepseek-flash",
        "the stored row says `qwen-3.8-flash-next`: the picker and `letibot --sessions` would go \
         on describing a deepseek session as a local one after the daemon restarts"
    );

    registry.close();
}
