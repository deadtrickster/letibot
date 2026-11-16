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

/// rano's 19:05: a daemon restart reopens a session whose transcript holds "5h at 100%, resets
/// T". A reading of the same window seconds off says nothing; the next window at 85% says one.
#[test]
fn a_restart_does_not_say_a_stored_crossing_again() {
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "usage-reopen-{}-{}",
        std::process::id(),
        letibot_sessionlog::event::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.join("sessions.db"));
    cfg.session_id = "s-usage-reopen".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let th = letibot_provider::usage_warn::Thresholds::default();
    let five_h = |used, reset| {
        [letibot_provider::usage_poll::Window {
            name: "5h".into(),
            used,
            reset_ms: Some(reset),
        }]
    };
    let now = letibot_sessionlog::event::now_ms();
    let t = now + 2 * 3_600_000;
    {
        let mut h = Harness::open(&parts, cfg.clone(), Hub::new("s-usage-reopen"))
            .expect("the session opens");
        h.usage_said
            .lock()
            .unwrap()
            .hear("kimi-code-global", &five_h(1.0, t), &th, now);
        h.flush_usage_notices().expect("the row is written");
    }
    let mut h =
        Harness::open(&parts, cfg, Hub::new("s-usage-reopen")).expect("the session reopens");
    let before = h.items().len();
    assert!(before > 0, "the reopened session holds its transcript");
    h.seed_usage_said("kimi-code-global", &th);
    let mut heard = |used, reset| {
        h.usage_said
            .lock()
            .unwrap()
            .hear("kimi-code-global", &five_h(used, reset), &th, now);
        h.flush_usage_notices().expect("rows are written");
    };
    heard(1.0, t + 2000);
    heard(0.85, t + 5 * 3_600_000);
    let said: Vec<String> = h.items()[before..]
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { parts, .. } => Some(format!("{parts:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("5h at 85%"), "{said:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
