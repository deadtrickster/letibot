//! A session comes back at the point it was put at.

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

/// A restart brings back the LATER of the two rows. A project moved after the session was put
/// somewhere is the operator's newest word for that project, so the session comes back at it - and
/// says why. Observed live: db-fuzz had been put at `allow-all`, the project's row was later set to
/// `automode`, and the restart brought the session back at `allow-all`.
#[test]
fn a_project_row_written_after_the_sessions_own_wins_at_reopen() {
    let dir = TempDir::new("later-project-row");
    let ws = dir.0.join("ws");
    let cfg = config(&dir, "s-later");
    let db = cfg.store.clone().unwrap();
    let mut modes = ModeStore::load(&db);
    modes.set(&ws, Mode::WRITES_ALLOWED).unwrap();
    modes.set_session("s-later", Mode::READ_ONLY, None).unwrap();
    // The operator moves the PROJECT afterwards - from another session of it, or from `/mode` on
    // a root. Only the project's row is touched here, which is the case that came back wrong.
    // A clock guard, not a wait for work: the two rows must be stamped apart, so the later write is
    // the later `set_at`.
    std::thread::sleep(std::time::Duration::from_millis(5));
    modes.set(&ws, Mode::ALWAYS_ASK).unwrap();

    let parts = Parts::load(&cfg).expect("parts");
    let h = Harness::open(&parts, cfg, Hub::new("s-later")).expect("opens");
    assert_eq!(
        h.config().mode.name,
        Mode::ALWAYS_ASK.name,
        "the session's older row overruled the project's newer one"
    );

    // And the other way round: the session's own row, written after the project's, still wins.
    let cfg = config(&dir, "s-earlier");
    let mut modes = ModeStore::load(&db);
    modes.set(&ws, Mode::ALWAYS_ASK).unwrap();
    // The same clock guard the other way round: the session's row has to be the LATER one.
    std::thread::sleep(std::time::Duration::from_millis(5));
    modes
        .set_session("s-earlier", Mode::READ_ONLY, None)
        .unwrap();
    let parts = Parts::load(&cfg).expect("parts");
    let h = Harness::open(&parts, cfg, Hub::new("s-earlier")).expect("opens");
    assert_eq!(h.config().mode.name, Mode::READ_ONLY.name);
}
