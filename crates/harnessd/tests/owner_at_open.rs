//! **A session records itself before anything it creates** (`docs/lifecycle.md` G1's first
//! done-when, the half a harness can prove): opening a package session writes one owner row and its
//! root with the stamp in it, and reopening the same session updates that row instead of adding a
//! second.
//!
//! One test, because `XDG_*` and `LETIBOT_GC_ROOT` are process-global: a sibling test would decide
//! where this one's owner root lands.

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::harness::Harness;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{OwnerKind, OwnerState, Store};

#[test]
fn a_package_session_writes_its_owner_row_and_root_before_anything_it_creates() {
    let scratch = letibot_sessionlog::testing::scratch("letibot-owner-at-open");
    let root = scratch.join("box");
    let workspace = root.join("ws");
    let gc = root.join("gc");
    let data = root.join("data");
    let run = root.join("run");
    for d in [&workspace, &gc, &data, &run] {
        std::fs::create_dir_all(d).expect("a fixture directory");
    }
    // SAFETY: this binary has ONE test, and every value here is a path in the fixture - so nothing
    // can reach the operator's store, runtime directory or owner roots.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", &data);
        std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
        std::env::set_var("XDG_RUNTIME_DIR", &run);
        std::env::set_var("HOME", &root);
        std::env::set_var("LETIBOT_GC_ROOT", &gc);
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_STORE");
        std::env::remove_var("LETIBOT_MODE");
    }

    let store_path = root.join("sessions.db");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.session_id = "s-owner-at-open".into();
    // The byte vocabulary, so this runs on a machine with no model file at all.
    cfg.vocab_gguf = None;
    cfg.store = Some(store_path.clone());
    // A daemon the bridge started for a brief is a PACKAGE - `--parent-seat` is that marker.
    cfg.parent_seat = Some("cc@ws".into());
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");

    let registry = Registry::new();
    let open = |cfg: Config| {
        let hub = registry.new_hub(cfg.session_id.clone());
        Harness::open_with_registry(&parts, cfg, hub, None, None, registry.clone())
    };
    let _h = open(cfg.clone()).expect("the session opens");

    let store = Store::open(&store_path).expect("the store");
    let owner = store
        .owner("s-owner-at-open")
        .expect("a read")
        .expect("the owner row is written at open");
    assert_eq!(owner.kind, OwnerKind::Package, "a package's daemon");
    assert_eq!(owner.state, OwnerState::Live);
    assert_eq!(
        owner.repo.as_deref(),
        Some(workspace.display().to_string().as_str()),
        "the owner names the checkout it works against"
    );
    assert_eq!(
        owner.idle_ttl_ms,
        Some(30 * 60 * 1000),
        "a package idles out"
    );

    // The root and its stamp: what a reaper with no store reads.
    let root_dir = gc
        .join(letibot_tools::lifecycle::box_id())
        .join("s-owner-at-open");
    let stamp = std::fs::read_to_string(root_dir.join("owner.json"))
        .expect("the owner root carries its stamp");
    let v: serde_json::Value = serde_json::from_str(&stamp).expect("one object");
    assert_eq!(v["owner"], "s-owner-at-open");
    assert_eq!(v["kind"], "package");
    assert_eq!(v["box_id"], letibot_tools::lifecycle::box_id());

    // Reopening the same session is the same row, not a second one, and the creation is kept.
    drop(_h);
    let _again = open(cfg).expect("the session reopens");
    let store = Store::open(&store_path).expect("the store");
    let owners = store.owners().expect("a read");
    assert_eq!(
        owners.iter().filter(|o| o.id == "s-owner-at-open").count(),
        1,
        "a reopen added a second owner row: {owners:#?}"
    );
    let back = store.owner("s-owner-at-open").unwrap().unwrap();
    assert_eq!(back.created_ms, owner.created_ms, "the creation is a fact");
    assert_eq!(back.state, OwnerState::Live, "a resume is live again");
}
