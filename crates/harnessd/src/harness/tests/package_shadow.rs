//! A package is a normal child and gets no shadow, however its daemon was started. The session
//! is created the way the socket creates one for `leticode_start`, so the origin is the opener's.

use super::*;
use letibot_sessionlog::origin::{MCP_IDENTITY, SessionOrigin};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::store::Store;

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A model that answers every turn with one line.
struct Says;

impl letibot_backend::MessagesBackend for Says {
    fn caps(&self) -> letibot_backend::BackendCaps {
        letibot_backend::BackendCaps::METERED_API
    }
    fn name(&self) -> &str {
        "deepseek"
    }
    fn model(&self) -> &str {
        "fake"
    }
    fn authority(&self) -> String {
        "a fake provider".into()
    }
    fn complete(
        &self,
        _req: &letibot_backend::TurnRequest<'_>,
        _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> letibot_backend::StreamFlow,
    ) -> Result<letibot_backend::Completion, letibot_backend::BackendError> {
        Ok(letibot_backend::Completion {
            text: "done.".into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish: letibot_backend::Finish::Stop,
            cost: letibot_backend::TurnCost {
                meter: letibot_backend::Meter::Metered,
                prompt_tokens: 10,
                cached_tokens: 0,
                generated_tokens: 1,
                wall_ms: 1,
                micros_usd: None,
            },
            raw_usage: None,
            thoughts: Vec::new(),
        })
    }
}

/// Open a top-level session created by `identity` over the socket, with `[shadow]` on, run one
/// turn, and answer whether it watches and whether `<id>-shadow` was stored.
fn opened_by(tag: &str, identity: &str) -> (bool, bool) {
    let dir = Scratch(
        letibot_sessionlog::testing::scratch_root()
            .join(format!("pkg-shadow-{tag}-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let id = format!("s-pkg-shadow-{tag}");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = id.clone();
    cfg.vocab_gguf = None;
    cfg.shadow = crate::shadow::config::ShadowConfig::from_lines(
        letibot_provider::keys::ShadowLines {
            enabled: vec!["turn_end".into()],
            ..Default::default()
        },
        "local".into(),
    )
    .unwrap();
    assert!(cfg.parent_seat.is_none() && cfg.parent_session_id.is_none());
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let registry = Registry::new();
    let wiring = SessionWiring {
        workspace: dir.0.display().to_string(),
        ..SessionWiring::default()
    };
    let hub = registry
        .create_as(
            &id,
            "",
            wiring,
            None,
            SessionOrigin::of_opener("remote", identity),
        )
        .expect("created");
    let mut h = Harness::open_with_registry(&parts, cfg, hub, None, None, registry)
        .expect("the session opens");
    h.provider = Some(Box::new(Says));
    h.submit_operator("hello").expect("a turn runs");
    h.tend_shadow();
    let watches = crate::shadow::watches(h.config());
    let stored = Store::open(&dir.0.join("sessions.db"))
        .expect("the store")
        .session(&crate::shadow::session_id(&id))
        .expect("the row reads")
        .is_some();
    (watches, stored)
}

#[test]
fn a_session_leticode_start_opens_has_no_shadow() {
    assert_eq!(
        opened_by("package", MCP_IDENTITY),
        (false, false),
        "a package's session neither watches nor stores a shadow session"
    );
}

#[test]
fn an_operator_top_level_session_still_has_its_shadow() {
    assert_eq!(
        opened_by("operator", "dead@tty"),
        (true, true),
        "the operator's session with [shadow] on is watched"
    );
}
