//! The review queue's seats at the operator's point: a project at `allow-all` (as `/mode` leaves
//! it) opens a gatekeeper-seated session there, its probes inside the clone run without a card, and
//! a write outside it still asks.

use std::path::Path;

use letibot_tools::{Access, GateCall, GateDecision};

use crate::config::{Config, ProviderConfig, Seat};
use crate::sessions::Sessions;

fn config(session: &str, ws: &Path) -> Config {
    let mut cfg = Config::for_this_box(ws);
    cfg.seat = Seat::Gatekeeper;
    cfg.session_id = session.to_string();
    cfg.vocab_gguf = None;
    cfg.allow_bash = true;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg.web_search = None;
    cfg.permission = Vec::new();
    let scratch = letibot_sessionlog::testing::scratch_root();
    cfg.store = Some(scratch.join(format!("{session}.db")));
    cfg.socket = scratch.join(format!("{session}.sock"));
    cfg
}

fn bash<'a>(args: &'a serde_json::Value, ws: &'a str) -> GateCall<'a> {
    GateCall {
        name: "bash",
        access: Access::Exec,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: ws,
        target_exists: None,
        scripts: &[],
        preview: &[],
    }
}

#[test]
fn a_reviewer_at_the_operators_allow_all_probes_its_clone_unasked_and_asks_beyond_it() {
    let ws = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-reviewer-mode-{}-{}",
        std::process::id(),
        letibot_sessionlog::registry::now_ms()
    ));
    std::fs::create_dir_all(&ws).expect("a clone");
    let ws = std::fs::canonicalize(&ws).expect("canonical");
    let tag = ws.file_name().unwrap().to_string_lossy().into_owned();
    let cfg = config(&format!("s-{tag}"), &ws);
    let parts: &'static crate::Parts = Box::leak(Box::new(
        crate::Parts::load(&cfg).expect("the byte vocabulary loads"),
    ));
    // The operator's `/mode allow-all`, answered: the project row and its consent.
    {
        let mut store = parts.mode_store.write().unwrap();
        store
            .set(&ws, letibot_tools::mode::Mode::ALLOW_ALL)
            .expect("the row");
        store
            .set_consent(&ws, "2026-11-15T00:00:00Z")
            .expect("consent");
    }
    let registry = letibot_sessionlog::Registry::new();
    registry
        .create(cfg.session_id.clone(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry");
    let sessions = Sessions::open_first(parts, cfg.clone(), registry).expect("the session opens");
    let h = sessions.harness_of(&cfg.session_id).expect("held");
    assert_eq!(
        h.config().mode.name,
        letibot_tools::mode::Mode::ALLOW_ALL_HERE.name,
        "the seat opens at the operator's point"
    );
    let at = ws.display().to_string();
    let admit = |cmd: &str| {
        let args = serde_json::json!({ "command": cmd });
        h.runtime
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admit(&bash(&args, &at))
    };
    // No head is attached, so anything that asks fails closed instead of admitting.
    assert_eq!(admit("clippy-driver --version"), GateDecision::Admit);
    assert_eq!(
        admit(&format!(
            "python3 -c \"open('{at}/mc.rs','w').write('fn main() {{}}')\""
        )),
        GateDecision::Admit,
        "a write inside the clone is the probe's own"
    );
    // The shared /tmp is every process's, so writing there is not a probe.
    assert!(
        matches!(
            admit("python3 -c \"open('/tmp/mc.rs','w').write('fn main() {}')\""),
            GateDecision::Refuse { .. }
        ),
        "a write into /tmp asks"
    );
    assert!(
        matches!(
            admit("python3 -c \"open('/home/dead/mc.rs','w').write('x')\""),
            GateDecision::Refuse { .. }
        ),
        "a write outside the clone still asks"
    );
    assert!(
        matches!(admit("git push origin HEAD"), GateDecision::Refuse { .. }),
        "a push still asks"
    );
    let _ = std::fs::remove_dir_all(&ws);
    if let Some(db) = &cfg.store {
        let _ = std::fs::remove_file(db);
    }
}
