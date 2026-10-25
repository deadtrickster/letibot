//! **A plugin pane, end to end, through the REAL wasm** — W4 of `docs/plugins-slice1.md`.
//!
//! The chain this file proves is the whole of the slice's wiring, and it is deliberately the real
//! one at every step: a daemon (`Daemon::serve`) serving one session (`Sessions::open_first`), a
//! head's `ClientFrame::PaneOpen` on a real socket, the daemon's `PaneDriver` for that session,
//! `letibot_plugins::Stack` resolving `pane_dashboard`, the rano `Loader` compiling
//! `plugins/examples/plan-dashboard/plugin.wasm`, the pane's two answers, and
//! `SessionEvent::PluginPane` back up the same socket.
//!
//! **Nothing here is a stub but the model**, and the model is never asked anything: the session
//! is opened on the byte vocabulary (`vocab_gguf = None`) with a provider that is named and never
//! called, the way `idle_clock.rs` and `no_vocab.rs` do — so this runs on a box with no GGUF, no
//! server and no key.
//!
//! # What is asserted, and why these two frames
//!
//! C5's example asks for its file and draws nothing until it has it: the first answer is the
//! `loading` frame — `plan`, one unavailable panel, `"look":{"read":".letibot/status.json"}` — and
//! the second is the dashboard, whose title carries the release the status file names and whose
//! panels are the plan's tiers, stages, agents and hosts. So the two frames ARE the proof that a
//! look ran and its result came back into the pane:
//!
//! * the first `PluginPane` is titled `plan` and has no `tiers` panel;
//! * the last is titled `plan · v0.2.0` (the name in `status.example.json`) and has one.
//!
//! # What it copies
//!
//! `plugin.toml` and `plugin.wasm` from `plugins/examples/plan-dashboard/`, and that directory's
//! `status.example.json` as the project's `.letibot/status.json` — exactly what a person wanting
//! this dashboard on their own project copies. The module is committed (252 KiB of it is
//! serde_json, see `.letibot/notes/the-example-plugin-weighs-252k.md`), so nothing here compiles
//! wasm: the `CargoBuilder` is the daemon's production one and it is never reached, because the
//! file on disk is what the gate would have produced.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::daemon::Daemon;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;

/// The pane the layer exports: `<manifest name>/<hook name>` (C4).
const PANE: &str = "plan-dashboard/dashboard";

/// The release `status.example.json` names — the second frame's title is built from it.
const RELEASE: &str = "v0.2.0";

/// How long a head waits for the frames. Generous: the module is compiled once here, and a loaded
/// runner may take a second over cranelift's first pass.
const PATIENCE: Duration = Duration::from_secs(30);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
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

/// The example plugin's own directory in this checkout.
fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/examples/plan-dashboard")
        .canonicalize()
        .expect("the example plugin is in this checkout")
}

/// **A project that has installed the example**: the layer where a project-scope plugin lives, and
/// the status file the pane reads.
fn project(dir: &Path) -> PathBuf {
    let root = dir.join("project");
    let layer = root.join(".letibot/plugins/plan-dashboard");
    std::fs::create_dir_all(&layer).expect("the plugin directory");
    let example = example();
    std::fs::copy(example.join("plugin.toml"), layer.join("plugin.toml")).expect("a manifest");
    std::fs::copy(example.join("plugin.wasm"), layer.join("plugin.wasm")).expect("a module");
    std::fs::copy(
        example.join("status.example.json"),
        root.join(".letibot/status.json"),
    )
    .expect("a status file");
    root
}

/// A session on the byte vocabulary with a provider that is named and never called: no GGUF, no
/// server, no key — and no turn in this test at all.
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

/// **What a head sees**: every `PluginPane` view the log published to it, in order.
fn pane_opened(socket: &Path, session: &str) -> Vec<serde_json::Value> {
    let (mut client, _hello, mut reader) =
        HeadClient::attach(socket, session, 0, "test", "plugins-pane", Caps::default())
            .expect("attaching to the daemon");
    // The reader on its own thread, which is the shape a head has: the frames arrive as they are
    // published, and the thread that asked keeps asking.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            match reader.read() {
                Ok(ServerFrame::Event(env)) => {
                    if tx.send(env.event).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
    client.pane_open(PANE).expect("asking for the pane");

    let mut seen = Vec::new();
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        let Ok(event) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        if let SessionEvent::PluginPane { pane, view } = event {
            assert_eq!(pane, PANE);
            let titled = view["title"].as_str().unwrap_or_default().to_string();
            seen.push(view);
            // **The dashboard is the frame this test is for**, so the wait ends on it rather than
            // on a count nobody outside the plugin can know — the pane is announced on attach and
            // answered on open, so there is more than one frame on the way.
            if titled.starts_with("plan · ") {
                break;
            }
        }
    }
    seen
}

/// **The 252 KiB module, the real plugin, the spec's own input and answer.**
#[test]
fn the_example_dashboard_answers_its_look_through_the_daemons_loader() {
    let dir = TempDir::new("plugins-pane");
    let workspace = project(dir.path());
    let store = dir.path().join("sessions.db");
    let session = "s-plugins-pane";
    let cfg = config(&store, session, &workspace);
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let registry = Registry::new();
    registry
        .create(session.to_string(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let socket = cfg.socket.clone();
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    // **The session's panes are announced before anybody opens one** (C4), so a head's list has
    // titles — the discovery half of this test is the same events the open half produces.
    let hub = registry.get(session).expect("the session's hub");
    assert!(
        hub.retained()
            .iter()
            .any(|e| matches!(&e.event, SessionEvent::PluginPane { pane, .. } if pane == PANE)),
        "the session opened without announcing its panes"
    );

    let driver = {
        let registry = registry.clone();
        let socket = socket.clone();
        std::thread::spawn(move || {
            let seen = pane_opened(&socket, session);
            // **Always**: closing the registry is what ends `Daemon::run`, so a failure has to be
            // an assertion rather than a hung test.
            registry.close();
            seen
        })
    };
    daemon.run(&mut sessions, |_, _, _| {});
    let seen = driver.join().expect("the head's thread");

    assert!(
        !seen.is_empty(),
        "the pane answered nothing at all — the panes list would be empty and `/pane {PANE}` would \
         say this session has never heard of it"
    );
    let first = &seen[0];
    assert_eq!(
        first["title"], "plan",
        "the first frame is the one that asks for the file: {first}"
    );
    assert!(
        first["look"]["read"] == ".letibot/status.json",
        "the first frame asks for the project's status file: {first}"
    );
    assert!(
        first["panels"]
            .as_array()
            .is_some_and(|p| p.iter().all(|panel| panel["id"] != "tiers")),
        "nothing has been read yet, so there is no tiers panel: {first}"
    );

    let last = seen.last().expect("at least one view");
    assert_eq!(
        last["title"],
        format!("plan · {RELEASE}"),
        "the dashboard drew the release the status file names — a title of `plan` means the look \
         never ran or its result never came back: {last}"
    );
    let tiers = last["panels"]
        .as_array()
        .expect("the dashboard has panels")
        .iter()
        .find(|p| p["id"] == "tiers")
        .unwrap_or_else(|| panic!("no `tiers` panel in {last}"));
    assert_eq!(tiers["key"], "t");
    assert!(
        tiers["denominator"].as_str().is_some_and(|d| !d.is_empty()),
        "every bar says its denominator (C3): {tiers}"
    );
    // **And the view this host publishes is one it can drive**: the daemon validated it before
    // publishing, so the head's own check is the second opinion rather than the only one.
    let view: letibot_sessionlog::pane::View =
        serde_json::from_value(last.clone()).expect("a C3 view");
    view.validate().expect("a view the host's keys can drive");
}
