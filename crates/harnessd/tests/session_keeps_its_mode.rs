//! **A session comes back at the point it was put at** — the operator, 2026-10-12: *"and again
//! peerdb allow-all lost after restart/reseat"*. The point a session was moved to with `/mode` is
//! the session's own row in the store, and an open reads it before the project's: so a restart,
//! or a reseat, brings it back there whatever the project row says by then — or whether another
//! daemon wrote that row away.

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::modes::ModeStore;
use letibot_harnessd::{Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tools::mode::Mode;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("ws")).expect("a temp dir");
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(dir: &TempDir, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box(dir.0.join("ws"));
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = session_id.to_string();
    // A provider that is never called, and no GGUF: nothing here runs a turn.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

#[test]
fn a_session_opens_at_its_own_point_over_its_projects() {
    let dir = TempDir::new("own-mode");
    let cfg = config(&dir, "s-own");
    let db = cfg.store.clone().unwrap();
    let mut modes = ModeStore::load(&db);
    // The project row says one thing; the session was put somewhere else.
    modes.set(&dir.0.join("ws"), Mode::READ_ONLY).unwrap();
    modes
        .set_session("s-own", Mode::WRITES_ALLOWED, None)
        .unwrap();

    let parts = Parts::load(&cfg).expect("parts");
    let h = Harness::open(&parts, cfg, Hub::new("s-own")).expect("opens");
    assert_eq!(
        h.config().mode.name,
        Mode::WRITES_ALLOWED.name,
        "the session's own point, not its project's"
    );

    // A session nobody moved opens at its project's point.
    let cfg = config(&dir, "s-other");
    let parts = Parts::load(&cfg).expect("parts");
    let h = Harness::open(&parts, cfg, Hub::new("s-other")).expect("opens");
    assert_eq!(h.config().mode.name, Mode::READ_ONLY.name);
}
