//! **A daemon for a cloud provider needs no GGUF** — and one with neither opens, and refuses
//! the turn by name.
//!
//! The operator's ask: *"say i want to just install leticode, give it my deepseek/glm key
//! and continue"*. Before this, `Parts::load` read a GGUF on every start and the engine
//! checked the dialect's control tokens against it, so a box with only a key could not
//! start a session at all — and none of llama.cpp's bundled vocabularies passed that check
//! (measured 2026-10-07 on a Mac: `qwen2`, `qwen35`, `deepseek-llm`).
//!
//! These need no model file, no server and no key: the provider is named, never called.

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::dialect::Dialect;
use letibot_harnessd::harness::{Parts, engine_for};

fn provider_cfg(dialect: Dialect) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = dialect;
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

/// Both dialects' whole control table and stop list resolve on the byte vocabulary —
/// the startup check that refused every bundled GGUF — and the engine is built.
#[test]
fn a_provider_session_opens_on_the_byte_vocabulary_for_both_dialects() {
    for dialect in [Dialect::Qwen, Dialect::Glm] {
        let cfg = provider_cfg(dialect);
        let parts = Parts::load(&cfg).unwrap_or_else(|e| panic!("{dialect:?}: {e:?}"));
        assert!(parts.vocab.is_bytes(), "{dialect:?}");
        assert!(parts.vocab.source().starts_with("bytes:"));
        let engine = engine_for(&parts, &cfg).unwrap_or_else(|e| panic!("{dialect:?}: {e:?}"));
        assert!(engine.vocab().is_bytes());
        assert!(!engine.control().is_empty());
    }
}

/// The two dialects reserve different literals, so their byte vocabularies are different
/// vocabularies — the identity a resume compares.
#[test]
fn each_dialect_has_its_own_byte_vocabulary() {
    let q = Parts::load(&provider_cfg(Dialect::Qwen)).unwrap();
    let g = Parts::load(&provider_cfg(Dialect::Glm)).unwrap();
    assert_ne!(q.vocab.source(), g.vocab.source());
}

/// **No GGUF and no provider: the daemon opens, and the TURN is refused by name.**
///
/// This was a refusal at load — the daemon's half of the launcher's model gate, so a box with
/// no GGUF and no key could not start a session at all. The operator, 2026-10-18: *"idk why i
/// even have to do --provider. horrible ux - we already have models chooser and keys asker"*.
/// Now the session opens on the byte vocabulary with no model chosen, the head's chooser is the
/// way on, and a turn that reaches the daemon anyway (a one-shot, an older head) is refused
/// naming `/models` — before anything is sent, because a llama-server cannot read byte ids.
#[test]
fn no_vocabulary_and_no_provider_opens_and_refuses_the_turn_by_name() {
    use letibot_harnessd::Harness;
    use letibot_sessionlog::hub::Hub;
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-no-model-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mut cfg = provider_cfg(Dialect::Qwen);
    cfg.provider = None;
    cfg.store = Some(dir.join("s.db"));
    cfg.session_id = "none".into();
    let parts = Parts::load(&cfg).unwrap_or_else(|e| panic!("no model chosen still opens: {e:?}"));
    assert!(parts.vocab.is_bytes());
    let mut h =
        Harness::open_with(&parts, cfg, Hub::new("none"), None, None).expect("the session opens");
    let e = match h.submit("hello") {
        Ok(_) => panic!("a turn went to a model nobody chose"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("no model is chosen"), "{e}");
    assert!(e.contains("/models"), "the refusal names the way on: {e}");
    let _ = std::fs::remove_dir_all(&dir);
}
