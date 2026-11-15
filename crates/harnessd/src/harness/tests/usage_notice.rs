//! A usage threshold the poll heard is one agent row in the session, written once.

use super::*;
use letibot_transcript::TranscriptItem;

#[test]
fn a_crossing_is_one_row_the_model_reads() {
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "usage-notice-{}-{}",
        std::process::id(),
        letibot_sessionlog::event::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.join("sessions.db"));
    cfg.session_id = "s-usage-notice".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let mut h = Harness::open(&parts, cfg, Hub::new("s-usage-notice")).expect("the session opens");
    let th = letibot_provider::usage_warn::Thresholds::default();
    let w = [letibot_provider::usage_poll::Window {
        name: "5h".into(),
        used: 0.82,
        reset_ms: None,
    }];
    let before = h.items().len();
    for _ in 0..2 {
        h.usage_said.lock().unwrap().hear("kimi-code", &w, &th, 0);
        h.flush_usage_notices().expect("the row is written");
    }
    let said: Vec<String> = h.items()[before..]
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { parts, .. } => Some(format!("{parts:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("[usage] kimi-code 5h at 82%"), "{said:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
