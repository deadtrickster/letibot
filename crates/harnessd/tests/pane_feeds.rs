//! **A pane is handed the feeds it declared** — `docs/plugins-plan.md` §4.9, from the daemon's side.
//!
//! The head has built the `sessions` feed for its own in-process session list since feeds landed
//! (`crates/tui/src/app/sessions_pane.rs`). **The daemon did not**: its C3 input carried `v`,
//! `state`, `look`, `event` and `size`, so a layer on disk that declared `feeds = ["sessions"]` was
//! handed nothing and drew an empty pane — a declaration the host does not honour, which on a
//! screen reads as a broken plugin rather than as a host that never had the feature.
//!
//! These tests are the daemon's half, through the public seams and nothing else: a `PluginHost`
//! with a `SessionFeed` of its own, a `Loader` that records the input it was called with, and a
//! `PaneDriver::open` — the same call `ClientFrame::PaneOpen` makes. What is asserted is the input
//! the module was really given, because that is what a plugin sees and all it can see.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_harnessd::plugins::PluginHost;
use letibot_harnessd::plugins::feed::SessionFeed;
use letibot_plugins::{Capability, CargoBuilder, Loader, Module};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::PaneDriver;
use serde_json::{Value, json};

/// A session list the fixture hands the host: two sessions, one of them a subagent.
fn fixture_rows() -> Vec<Value> {
    vec![
        json!({"id": "s-1", "title": "the habr article", "parent": null, "live": true,
               "running": true, "heads": 1, "items": 6, "stored": 6, "model": "qwen",
               "workspace": "/p/letibot", "waits_on": 0, "nags_paused": false,
               "moved_ms": 1_000, "repo": "/p/letibot"}),
        json!({"id": "s-1-sub-2", "title": "read the repo", "parent": "s-1", "live": true,
               "running": false, "heads": 0, "items": 2, "stored": 2, "model": "qwen",
               "workspace": "/p/letibot", "waits_on": 0, "nags_paused": false,
               "moved_ms": 900, "repo": "/p/letibot"}),
    ]
}

/// The feed the daemon would build from its registry, stood in for.
struct Fixture;

impl SessionFeed for Fixture {
    fn rows(&self) -> Vec<Value> {
        fixture_rows()
    }
}

/// **A module that answers a pane and remembers what it was asked** — the whole of what these
/// tests need from an engine, without one: the input is the contract, and a wasm call would only
/// add a compiler to the way of reading it.
struct Recording {
    seen: Arc<Mutex<Vec<String>>>,
    /// The hooks this module exports.
    hooks: Vec<String>,
}

impl Module for Recording {
    fn hooks(&self) -> Vec<String> {
        self.hooks.clone()
    }

    fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(input.to_string());
        }
        // A view this host validates: one panel, one line, so the frame published is a real one.
        Ok(Some(
            json!({
                "panels": [{"id": "nicks", "summary": [{"lines": [
                    {"label": "the nick list", "value": "2 here"}
                ]}]}],
            })
            .to_string(),
        ))
    }

    fn fuel(&self) -> u64 {
        3
    }
}

struct RecordingLoader {
    seen: Arc<Mutex<Vec<String>>>,
    hooks: Vec<String>,
}

impl Loader for RecordingLoader {
    fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        Ok(Box::new(Recording {
            seen: self.seen.clone(),
            hooks: self.hooks.clone(),
        }))
    }
}

/// A project with one layer whose manifest declares `feeds`, and the module it names.
fn layer(root: &Path, name: &str, hook: &str, feeds: &[&str]) -> PathBuf {
    let dir = root.join(".letibot").join("plugins").join(name);
    std::fs::create_dir_all(&dir).expect("the layer's directory");
    let feeds = feeds
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\npurpose = \"one pane, for the feed tests\"\n\
             hooks = [\"{hook}\"]\ncapabilities = []\nfeeds = [{feeds}]\ncontract = 1\n"
        ),
    )
    .expect("a manifest");
    std::fs::write(dir.join("plugin.wasm"), b"a module").expect("a module");
    dir
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "pane-feeds-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// **A host with the fixture feed and a recording loader**, and the inputs it was asked with.
fn host(
    root: &Path,
    hook: &str,
    feeds: Option<Arc<dyn SessionFeed>>,
) -> (PluginHost, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let host = PluginHost::discover(
        "s-plugins",
        root,
        None,
        None,
        Arc::new(RecordingLoader {
            seen: seen.clone(),
            hooks: vec![hook.to_string()],
        }),
        // Nothing is built here: the module on disk is the one the loader hands over.
        Arc::new(CargoBuilder),
    );
    let host = match feeds {
        Some(source) => host.with_sessions(source),
        None => host,
    };
    (host, seen)
}

/// The input the module was called with, parsed.
fn first_input(seen: &Arc<Mutex<Vec<String>>>) -> Value {
    let inputs = seen.lock().unwrap_or_else(|e| e.into_inner());
    let first = inputs.first().expect("the module was asked");
    serde_json::from_str(first).expect("the input is JSON")
}

/// **A pane that declares `sessions` is handed one**, built by the host, with the three fields only
/// a host can answer: `here` (the session whose pane this is), `now` (a wasm guest has no clock)
/// and each row's `moved_ms` and `repo`.
#[test]
fn a_pane_that_declares_the_sessions_feed_is_handed_one() {
    let root = tempdir("declared");
    layer(&root, "nicks", "pane_nicks", &["sessions"]);
    let (host, seen) = host(&root, "pane_nicks", Some(Arc::new(Fixture)));
    let hub = Hub::new("s-plugins");
    host.open("s-plugins", &hub, "nicks/nicks", (100, 40));

    let input = first_input(&seen);
    let feed = &input["feeds"]["sessions"];
    assert_eq!(feed["sessions"][0]["id"], "s-1", "the rows: {input}");
    assert_eq!(
        feed["sessions"][1]["parent"], "s-1",
        "a subagent is a row too"
    );
    assert_eq!(
        feed["here"], "s-plugins",
        "the pane belongs to a session, and that is what `here` means to it"
    );
    assert!(
        feed["now"].as_u64().unwrap_or(0) > 0,
        "the host's clock: a guest on wasm32 has none of its own"
    );
    assert_eq!(feed["sessions"][0]["moved_ms"], 1_000);
    assert_eq!(feed["sessions"][0]["repo"], "/p/letibot");
    assert!(
        feed["home"].as_str().is_some(),
        "the host's home, so a layer can draw `~/…`"
    );
    // And the contract version is the SDK's, not a stale copy: a layer that reads `v` to decide
    // whether it will be handed a feed must not be told *no* by a host that is about to hand it one.
    assert_eq!(input["v"], letibot_plugin::pane::V);
    // The pane answered, so the frame the head draws is a real one.
    let published: Vec<String> = hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::PluginPane { pane, .. } => Some(pane.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(published, vec!["nicks/nicks".to_string()]);
}

/// **A layer that declared no feed is handed none** — the sandbox on this side is the declaration,
/// so a pane that reads a feed it did not ask for does not get one.
#[test]
fn a_pane_that_declares_no_feed_is_handed_none() {
    let root = tempdir("undeclared");
    layer(&root, "quiet", "pane_quiet", &[]);
    let (host, seen) = host(&root, "pane_quiet", Some(Arc::new(Fixture)));
    let hub = Hub::new("s-plugins");
    host.open("s-plugins", &hub, "quiet/quiet", (100, 40));

    let input = first_input(&seen);
    assert_eq!(
        input["feeds"],
        json!({}),
        "the map is the manifest's, not the pane's"
    );
}

/// **A feed this host cannot build is ABSENT, not empty** — `[]` would tell a layer *there are
/// none*, and *this host cannot answer that yet* is a different fact that only the layer can draw.
#[test]
fn a_feed_this_host_cannot_build_is_absent_rather_than_empty() {
    let root = tempdir("planned");
    layer(&root, "watcher", "pane_watcher", &["monitors", "sessions"]);
    let (host, seen) = host(&root, "pane_watcher", Some(Arc::new(Fixture)));
    let hub = Hub::new("s-plugins");
    host.open("s-plugins", &hub, "watcher/watcher", (100, 40));

    let input = first_input(&seen);
    assert!(
        input["feeds"].get("monitors").is_none(),
        "the planned feed is not invented: {input}"
    );
    assert!(
        input["feeds"].get("sessions").is_some(),
        "and the one this host can build is there"
    );
}

/// **A host with no session list hands no `sessions` key at all**, so a pane says so on its screen
/// instead of drawing an empty list — which is the difference between a broken host and a daemon
/// with no sessions.
#[test]
fn a_host_with_no_session_list_hands_no_feed() {
    let root = tempdir("nofeed");
    layer(&root, "nicks", "pane_nicks", &["sessions"]);
    let (host, seen) = host(&root, "pane_nicks", None);
    let hub = Hub::new("s-plugins");
    host.open("s-plugins", &hub, "nicks/nicks", (100, 40));
    let input = first_input(&seen);
    assert_eq!(input["feeds"], json!({}));
}
