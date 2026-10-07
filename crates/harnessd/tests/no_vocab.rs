//! **A daemon for a cloud provider needs no GGUF** — and one without a provider is refused.
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

/// No GGUF and no provider: nothing could answer a turn, and the refusal names both ways
/// out rather than guessing a path.
#[test]
fn no_vocabulary_and_no_provider_is_refused_by_name() {
    let mut cfg = provider_cfg(Dialect::Qwen);
    cfg.provider = None;
    let e = match Parts::load(&cfg) {
        Ok(_) => panic!("a local session with no vocabulary was opened"),
        Err(e) => format!("{e:?}"),
    };
    assert!(e.contains("--vocab"), "{e}");
    assert!(e.contains("--provider"), "{e}");
}
