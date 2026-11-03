//! Two heads on one live session: the one that moves the mode is answered, and the one that
//! asked for nothing is TOLD - the rows a head draws are live rather than the photograph it
//! read at attach.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::daemon::Daemon;
use letibot_harnessd::modes::ModeStore;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use letibot_tools::mode::Mode;

/// How long a frame on its way may take. Generous: nothing here runs a turn, so the frames are
/// a condvar wake and a socket write, and this only has to outlast a loaded box.
const PATIENCE: Duration = Duration::from_secs(30);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("ws")).expect("a workspace");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A session with a provider that is never called and no GGUF: nothing here runs a turn, so no
/// key and no server are needed.
fn config(store: &Path, session: &str, workspace: &Path) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session.to_string();
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg.socket = store.with_extension("sock");
    cfg
}

/// The value of the `mode` row in the first `Settings` frame the still head is sent. The frames
/// come off its own thread: a read that blocks is a read this test cannot put a deadline on.
fn mode_row(rx: &std::sync::mpsc::Receiver<ServerFrame>) -> String {
    let began = Instant::now();
    while let Some(left) = PATIENCE.checked_sub(began.elapsed()) {
        match rx.recv_timeout(left) {
            Ok(ServerFrame::Settings { rows }) => {
                return rows
                    .iter()
                    .find(|r| r.key == "mode")
                    .map(|r| r.value.clone())
                    .unwrap_or_default();
            }
            // Everything else on the way - the snapshot's events, the `HeadAttached` of the
            // second head, the answer to the mover's own request - is not this head's subject.
            Ok(_) => {}
            Err(e) => panic!("the still head was not told in {PATIENCE:?}: {e}"),
        }
    }
    panic!("the still head was not told in {PATIENCE:?}");
}

/// The mover asks; the still head only reads. Returns what the still head's `mode` row said.
fn mover_and_a_head_that_never_asks(socket: &Path, session: &str) -> String {
    let (mut mover, _hello, _mover_r) =
        HeadClient::attach(socket, session, 0, "tui", "mover", Caps::default())
            .expect("the asking head attaches");
    let (_still, _hello, mut still_r) =
        HeadClient::attach(socket, session, 0, "tui", "still", Caps::default())
            .expect("the listening head attaches");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(frame) = still_r.read() {
            if tx.send(frame).is_err() {
                return;
            }
        }
    });

    // The premise: the head that is about to be told has sent nothing but its `Attach`.
    mover
        .set_mode(0, Mode::ALWAYS_ASK.name, false)
        .expect("the mode request goes out");
    mode_row(&rx)
}

/// Closing the registry is what ends `Daemon::run`. A guard rather than a call at the end of the
/// heads' thread, because the thread that panics is exactly the one that must still stop it.
struct EndsTheDaemon(Arc<Registry>);

impl Drop for EndsTheDaemon {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[test]
fn a_mode_one_head_moves_reaches_the_other_without_asking() {
    let dir = TempDir::new("settings-live");
    let store = dir.path().join("sessions.db");
    let workspace = dir.path().join("ws");
    let session = "s-settings-live";
    let cfg = config(&store, session, &workspace);
    // The session's own row decides where it opens: `read-only` needs nothing from the box, and
    // it is not the point this test moves to.
    let modes = ModeStore::load(&store);
    modes
        .set_session(session, Mode::READ_ONLY, None)
        .expect("a session row");
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let registry = Registry::new();
    registry
        .create(session.to_string(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let socket = cfg.socket.clone();
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
    let mut sessions = Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("opens");

    let driver = {
        let socket = socket.clone();
        std::thread::spawn(move || {
            // Bound before the work: closing the registry is what ends `Daemon::run` however the
            // head work ends, so a panic here is a failing assertion rather than a hung test.
            let _ends = EndsTheDaemon(registry.clone());
            mover_and_a_head_that_never_asks(&socket, session)
        })
    };
    daemon.run(&mut sessions, |_, _| {});
    let told = match driver.join() {
        Ok(told) => told,
        Err(payload) => std::panic::resume_unwind(payload),
    };

    assert_eq!(
        told,
        Mode::ALWAYS_ASK.name,
        "the head that never asked is still drawing the mode it read at attach"
    );
}
