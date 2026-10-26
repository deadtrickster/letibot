//! **A copy of a session that lives somewhere else is refused, and the listing says where.**
//!
//! `letibot --migrate SESSION --to HOST` moves a session's whole daemon to a host and marks the
//! copy it leaves behind (`docs/remote-hosts.md` RH4, `session.lives_on`). Two surfaces have to
//! follow that marker, and each is here:
//!
//! * **`--sessions`** says *lives on lab2x1* beside the row instead of offering it as a
//!   conversation this box is appending to;
//! * **`Harness::open` refuses it by name**, because a marked copy is not being written to, and a
//!   turn in it would be work typed into a conversation nobody else will ever see.
//!
//! The session opens on the **byte vocabulary with a provider** — `vocab_gguf: None`,
//! `provider: Some(..)` — so this runs on a machine with no model file at all, and no request
//! leaves the box: the refusal happens before anything could be sent.

use std::process::Command;

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::Harness;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};

const MOVED: &str = "s-migrated-1";
const HERE: &str = "s-here-1";

/// A store with two sessions: one that lives on a host, one that lives here.
fn store_with_a_moved_session(path: &std::path::Path) -> Store {
    let store = Store::open(path).expect("a store");
    let tokens = vec![1u32, 2, 3];
    let prefix = StablePrefixRecord {
        dialect_sha: "bb".repeat(32),
        system: "you are a harness".into(),
        tools_json: vec![r#"{"name":"read"}"#.into()],
        h_init: letibot_tokencore::ledger::hash_tokens(&tokens),
        tokens,
        vocab_source: "bytes".into(),
    };
    let prefix_id = store.put_stable_prefix(&prefix).expect("a prefix");
    for (id, on) in [(MOVED, Some("lab2x1")), (HERE, None)] {
        store
            .put_session(&SessionRecord {
                id: id.into(),
                title: Some(format!("{id}'s conversation")),
                model_id: "deepseek-chat".into(),
                dialect_sha: prefix.dialect_sha.clone(),
                workspace_root: "/tmp/ws".into(),
                owner: "dead".into(),
                role: Some("leticode".into()),
                approvers: vec![],
                parent_session_id: None,
            })
            .expect("a session row");
        store
            .put_transcript(&format!("{id}#t0"), id, &prefix_id)
            .expect("a transcript");
        if let Some(host) = on {
            store.set_lives_on(id, Some(host)).expect("the marker");
        }
    }
    store
}

/// **`--sessions` says where it went**, and leaves the row in the list — the rows are here, and
/// a move back is what removes the mark.
#[test]
fn the_listing_names_the_host_a_moved_session_lives_on() {
    let dir = std::env::temp_dir().join(format!("letibot-marker-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    let path = dir.join("sessions.db");
    store_with_a_moved_session(&path);

    let out = Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .arg("--store")
        .arg(&path)
        .arg("--list-sessions")
        .output()
        .expect("the listing ran");
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        listed.contains("[lives on lab2x1]"),
        "a moved session has to say so, or the listing offers a conversation nobody is \
         appending to:\n{listed}"
    );
    assert!(
        listed.contains("s-here-1")
            && !listed
                .lines()
                .any(|l| l.contains("s-here-1") && l.contains("lives on")),
        "a session that is here is not marked:\n{listed}"
    );
    assert!(
        listed.contains("s-migrated-1"),
        "the row stays in the list — the rows are here:\n{listed}"
    );
}

/// **A marked copy does not open**, and the refusal says where the session is and how to get
/// there. A warning would be read by nobody: the person who has to notice it is looking at the
/// conversation the model would be answering in.
#[test]
fn a_session_that_lives_on_a_host_is_refused_by_name() {
    let dir = std::env::temp_dir().join(format!("letibot-marker-open-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    let path = dir.join("sessions.db");
    store_with_a_moved_session(&path);

    let workspace = dir.join("ws");
    std::fs::create_dir_all(&workspace).expect("a workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.session_id = MOVED.into();
    cfg.store = Some(path.clone());
    // The byte vocabulary and a provider: no model file, and nothing is sent — the refusal is
    // before the first round.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");

    let registry = Registry::new();
    let hub = registry.new_hub(cfg.session_id.clone());
    registry
        .adopt(
            hub.clone(),
            cfg.session_id.clone(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: "127.0.0.1:1".into(),
                workspace: workspace.display().to_string(),
            },
            None,
        )
        .expect("the session is in the registry");
    let opened = Harness::open_with_registry(&parts, cfg, hub, None, None, registry);
    let said = match opened {
        Ok(_) => panic!("a copy that lives elsewhere must not open"),
        Err(e) => e.to_string(),
    };
    assert!(
        said.contains("lives on `lab2x1`"),
        "the refusal names the endpoint: {said}"
    );
    assert!(
        said.contains("--migrate") && said.contains("--to local"),
        "and the way back ({said})"
    );
    assert!(
        said.contains("Nothing was opened"),
        "and says nothing happened: {said}"
    );
}
