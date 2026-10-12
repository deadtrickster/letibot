//! **R92.24: "workspace gone" is said when a session opens or wakes on a missing workspace.**

use super::*;

fn scratch(tag: &str) -> std::path::PathBuf {
    let p = letibot_sessionlog::testing::scratch_root().join(format!(
        "{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn said_gone(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } if code == "workspace_gone" => {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_session_opening_on_a_gone_workspace_says_so() {
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let dir = scratch("r92-24-open");
    let gone = dir.join("retired-worktree");
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.join("sessions.db"));
    cfg.session_id = "r92-24-open".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    cfg.workspace = gone.clone();
    let hub = Hub::new("r92-24-open");
    let err = match Harness::open(&parts, cfg, hub.clone()) {
        Ok(_) => panic!("a session cannot open on a workspace that is gone"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("workspace gone"), "{err}");
    assert!(err.contains("retired-worktree"), "{err}");
    let said = said_gone(&hub);
    assert_eq!(said.len(), 1, "said on the session's hub: {said:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_wake_on_a_workspace_that_went_away_says_so_once() {
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let dir = scratch("r92-24-wake");
    let ws = dir.join("tree");
    std::fs::create_dir_all(&ws).unwrap();
    let mut cfg = Config::for_this_box(&ws);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.join("sessions.db"));
    cfg.session_id = "r92-24-wake".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let hub = Hub::new("r92-24-wake");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    std::fs::remove_dir_all(&ws).unwrap();
    let _ = h.wake();
    let _ = h.wake();
    let said = said_gone(&hub);
    assert_eq!(said.len(), 1, "said once: {said:?}");
    assert!(said[0].contains("workspace gone"), "{}", said[0]);
    let _ = std::fs::remove_dir_all(&dir);
}
