//! An offer row is in the record at its place, survives a reopen, and adds nothing to the prompt.

use super::*;
use letibot_transcript::{OfferKind, TranscriptItem};

fn offer() -> TranscriptItem {
    TranscriptItem::Offer {
        kind: OfferKind::Skill,
        title: "skill offered: rust-skills (0.41) · loaded".into(),
        body: "Run clippy.".into(),
    }
}

fn config(dir: &std::path::Path, id: &str, gguf: std::path::PathBuf) -> Config {
    let mut cfg = Config::for_this_box(dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.join("sessions.db"));
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    cfg
}

fn offers_in(h: &Hub) -> Vec<usize> {
    h.snapshot()
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i.item, Some(TranscriptItem::Offer { .. })))
        .map(|(n, _)| n)
        .collect()
}

#[test]
fn an_offer_row_adds_no_tokens_and_is_replayed_after_a_reopen() {
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "offer-row-{}-{}",
        std::process::id(),
        letibot_sessionlog::event::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let id = "s-offer-row";
    let cfg = config(&dir, id, gguf.clone());
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let hub = Hub::new(id);
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    h.note_to_model("first").expect("a row");
    let tokens = h.session.ledger.len();
    let before = h.items().len();
    let mut staged = vec![offer()];
    h.append_offer_rows(&mut staged).expect("the row is filed");
    assert!(staged.is_empty());
    h.note_to_model("second").expect("a row after it");
    assert_eq!(h.items().len(), before + 2);
    assert!(
        h.session.ledger.len() > tokens,
        "the second note renders tokens"
    );
    let after_offer = h.session.ledger.rows()[before].tok_len;
    assert_eq!(after_offer, 0, "the offer row renders no token");
    assert_eq!(offers_in(&hub), vec![before], "live, at its position");
    drop(h);

    let cfg = config(&dir, id, gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let hub = Hub::new(id);
    let h = Harness::open(&parts, cfg, hub.clone()).expect("the session reopens");
    let at = h
        .items()
        .iter()
        .position(|i| matches!(i, TranscriptItem::Offer { .. }));
    assert!(at.is_some(), "the store kept the row");
    assert_eq!(offers_in(&hub).len(), 1, "a head attaching later has it");
    let _ = std::fs::remove_dir_all(&dir);
}
