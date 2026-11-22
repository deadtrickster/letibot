//! A child's own row carries the park and the model it was moved to. `children.rs` restores both
//! when it opens the child, which is the door a reopened child and a fresh spawn both take.

use super::*;

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child opened a second time comes back parked on the model whose limit it was waiting for,
/// rather than unlimited and re-spending the refusal.
#[test]
fn a_childs_open_puts_back_the_park_and_the_model_its_row_holds() {
    let dir = Scratch::new("child-park");
    let path = dir.0.join("sessions.db");
    let open = |id: &str| {
        let mut cfg = Config::for_this_box(&dir.0);
        cfg.dialect = crate::Dialect::Qwen;
        cfg.store = Some(path.clone());
        cfg.session_id = id.into();
        cfg.parent_session_id = Some("s-parent".into());
        cfg.web_search = None;
        at_always_ask(&mut cfg);
        let parts = Parts::load(&cfg).expect("the tokeniser loads");
        Harness::open_with(
            &parts,
            cfg,
            Hub::new(id),
            Some(Box::new(letibot_tools::adjudicate::NoAdjudicator)),
            None,
        )
        .expect("the session opens")
    };

    let first = open("s-kid");
    assert!(first.store.is_some(), "the fixture has a store");
    drop(first);
    let store = letibot_tokencore::Store::open(&path).expect("the store opens");
    let until = letibot_sessionlog::event::now_ms() + 600_000;
    store
        .set_provider_choice("s-kid", Some("deepseek/deepseek-flash"))
        .unwrap();
    store
        .set_limit_state(
            "s-kid",
            Some(&format!(
                r#"{{"park":{{"until_ms":{until},"reset_ms":{until},"model":"glm","said":"usage limit reached"}}}}"#
            )),
        )
        .unwrap();
    drop(store);

    let mut again = open("s-kid");
    // The two calls `children.rs` makes at the child's open, in its order. The model half needs a
    // `providers.toml` naming `deepseek`, so what is pinned here is the park; the choice is read by
    // the same door a session's own resume uses.
    again.restore_provider_choice();
    again.restore_limit_state();

    let park = again.limit_park.clone().expect("the park came back");
    assert_eq!(park.model, "glm");
    assert_eq!(park.until_ms, until);
    let said: Vec<String> = again
        .hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } if code == "limit_park_restored" => {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("glm"), "{said:?}");
}
