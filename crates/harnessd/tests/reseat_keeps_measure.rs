//! **A verbatim `/reseat` keeps the provider's measurement**, so a restart can still read the
//! ledger in the provider's units.
//!
//! The operator's peerdb session, 2026-10-12: a `/reseat` (which forks the conversation onto
//! the new prompt with every row carried) cleared the stored measurement, the daemon was
//! restarted before a round measured again, and the session was folded on 2,431,232 ledger
//! bytes of a conversation deepseek had counted at around 600,000 — "after 2m tokens, how scale
//! can not be measured?". It had been measured on every round; the fork threw it away.
//!
//! On the byte vocabulary a cloud session uses, so it runs on a box with no model file.

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::store::Store;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

#[test]
fn a_verbatim_reseat_keeps_the_measurement_and_moves_its_anchor() {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-reseat-measure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("s.db");
    let cfg = config(&path, "keep");
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let hub = Hub::new("keep");
    let mut h = Harness::open_with(&parts, cfg, hub, None, None).expect("the session opens");

    // The measurement a round left: the provider's count of the prompt at this ledger length.
    let store = Store::open(&path).expect("the store");
    let at = h.tokens().len() as u64;
    let prefix_before = h.prefix_tokens().len() as i64;
    store
        .set_context("keep", Some(600_000), Some(1_000), Some(at))
        .expect("seed the measurement");

    // Message zero changes — what seating a new tool does — and the conversation is re-seated
    // onto it verbatim.
    h.config_mut().system = "a different base prompt entirely".into();
    h.reingest().expect("the re-seat");

    let row = store.session("keep").unwrap().expect("the session row");
    let delta = h.prefix_tokens().len() as i64 - prefix_before;
    assert_ne!(delta, 0, "the premise: the prefix changed");
    assert_eq!(
        row.context_tokens,
        Some(600_000),
        "the re-seat threw the provider's count away"
    );
    assert_eq!(
        row.context_ledger,
        Some((at as i64 + delta) as u64),
        "the measurement's anchor did not move with the new message zero ({delta})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
