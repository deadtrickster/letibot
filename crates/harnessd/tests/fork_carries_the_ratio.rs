//! A fork that REPLACES the conversation cannot keep the provider's count of the old one, but the
//! ratio it measured is not a fact about the old conversation: it is how many ledger tokens this
//! box's prompt takes per token the provider counted. That half rides the fork, so a restart before
//! any round reads the new base in the provider's units instead of falling back to its ledger bytes.

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::harness::ForkTail;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::store::Store;
use letibot_turn::CompactionOutcome;

const SESSION: &str = "fork-ratio";

fn config(store: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // The byte vocabulary: this file needs no GGUF, and the ledger's own size is then a plain count.
    cfg.vocab_gguf = None;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = SESSION.to_string();
    // A metered session, since it is the only kind with a ratio to carry. Nothing calls it.
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

fn outcome(summary: &str) -> CompactionOutcome {
    CompactionOutcome {
        turn_id: "test-turn".into(),
        summary: summary.into(),
        tool_calls: 0,
        truncated: false,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
        budget: None,
        fidelity: None,
    }
}

#[test]
fn a_fork_that_replaces_the_conversation_carries_the_ratio_into_the_stored_pair() {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-fork-ratio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("s.db");
    let cfg = config(&path);
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");

    // The measurement a finished round leaves: the provider's count and the ledger length it counted.
    let h = Harness::open_with(&parts, cfg.clone(), Hub::new(SESSION), None, None)
        .expect("the session opens");
    let store = Store::open(&path).expect("the store");
    let at = h.ledger_len() as u64;
    assert!(at > 1_000, "the premise: the prompt is a prompt");
    let counted = at / 4;
    store
        .set_context(SESSION, Some(counted), Some(0), Some(at))
        .expect("seed the measurement");
    drop(h);

    // The restart that reads it, and then a compaction: the conversation the pair describes is
    // replaced, and only a measurement of the NEW base would be exact.
    let mut h = Harness::open_with(&parts, cfg, Hub::new(SESSION), None, None)
        .expect("the session reopens");
    h.fork_to_summary(&outcome("decided A because B"), None, None, ForkTail::NONE)
        .expect("the fork lands");

    let row = store.session(SESSION).unwrap().expect("the session row");
    let base = h.ledger_len() as u64;
    let expected = (base as u128 * counted as u128 / at as u128) as u64;
    assert_eq!(
        row.context_ledger,
        Some(base),
        "the carried pair is anchored on the new base"
    );
    assert_eq!(
        row.context_tokens,
        Some(expected),
        "the new base in the provider's units, through the ratio the fork did not change"
    );
    assert!(
        expected < base / 2,
        "the premise: the ratio is nowhere near one ({expected} of {base})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
