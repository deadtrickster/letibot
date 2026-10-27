//! **The four hook kinds, through rano's real engine** — `docs/plugins-plan.md` §7 step 2.
//!
//! Every test of the four patch points before this one answered through a *table* of closures
//! (`crates/plugins`'s tests, and `crates/harnessd/src/plugins.rs`'s `TableLoader`): precedence, the
//! markers, the fail-down and the contracts are all measurable that way, and none of them needs a
//! JIT. What a table cannot say is that a module written in the ABI actually answers — that
//! `rano_abi` is 1, that a hook returns an 8-byte result block and not a pointer, that the JSON a
//! guest hands back is the JSON the contract parses. That is this file, and it is the whole of what
//! it adds: **the host is the real one, the loader is `RanoLoader`, and the module is bytes that
//! were compiled from wasm rather than faked.**
//!
//! # The fixture
//!
//! `tests/data/plugin-engine/` holds `plugin.toml`, `plugin.wasm` (867 bytes) and the guest's
//! source. The module is committed rather than built here because a test that shells out to a
//! compiler fails on a box without a wasm target, which is exactly what the `Loader` seam exists to
//! avoid everywhere else; `guest/lib.rs`'s header carries the one `rustc` line that rebuilds it.
//! Rano's own tests use WAT, but their `wat` dev-dependency is a crate this tree's `Cargo.lock`
//! does not have and the offline cache cannot resolve, and wasmtime's `wat` feature is off in the
//! build rano pins — so the fixture is the small built module the plan allows for.
//!
//! One assertion per kind, in §4's order: the call, the result, the notice, the condition.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_harnessd::plugins::PluginHost;
use letibot_harnessd::plugins::conditions::HostConditions;
use letibot_harnessd::plugins::rano::RanoLoader;
use letibot_plugins::CargoBuilder;
use letibot_sessionlog::hub::Hub;
use letibot_tools::exec::plugin_condition::PluginConditions as _;
use letibot_tools::runtime::Standing;
use serde_json::json;

/// The fixture: the layer directory the host discovers.
fn layer() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/plugin-engine")
}

/// A host whose one layer is the committed module, in the SESSION scope — which is a directory of
/// layer directories, so the fixture is reached through its own name.
fn host(tmp: &Path) -> Arc<PluginHost> {
    let scope = tmp.join("plugins");
    let dest = scope.join("engine");
    std::fs::create_dir_all(&dest).expect("the session's plugin dir");
    for file in ["plugin.toml", "plugin.wasm"] {
        std::fs::copy(layer().join(file), dest.join(file)).expect("the fixture");
    }
    // The builder is never reached: `discover` loads the module that is already there, and the
    // gate's step 1 — the only caller of a `Builder` — needs `plugin build` or `plugin load`.
    Arc::new(PluginHost::discover(
        "s-plugin-engine",
        tmp.join("workspace"),
        Some(scope),
        None,
        Arc::new(RanoLoader),
        Arc::new(CargoBuilder),
    ))
}

fn tempdir(what: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "letibot-plugin-engine-{what}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(path.join("workspace")).expect("a temp dir");
    path
}

fn hub() -> Arc<Hub> {
    Hub::new("s-plugin-engine")
}

fn a_call() -> letibot_plugin::Call {
    letibot_plugin::Call::new("bash", json!({"command": "cargo test"}))
}

/// **`on_tool_call`** (§4.1): the module answers a call, and the answer is the host's own verdict
/// type — so the ABI's result block, the JSON text and `ToolCallOut` all agree.
#[test]
fn a_call_is_answered_by_the_committed_module() {
    let tmp = tempdir("call");
    let host = host(&tmp);
    let answered = host
        .on_tool_call(&hub(), &a_call(), &Standing::default())
        .expect("the module answers `on_tool_call`");
    assert_eq!(
        answered.out.verdict(),
        letibot_plugin::tool_call::Verdict::Answer("the engine answered this call")
    );
    assert_eq!(answered.by, "s1", "the session scope's first layer");
    // And the host names that layer the way a person wrote it, which is what a result carries.
    assert_eq!(host.layer_sentence(&answered.by), "engine (session)");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **`on_tool_result`** (§4.2): the result is what the module appended, through the typed contract.
#[test]
fn a_result_is_amended_by_the_committed_module() {
    let tmp = tempdir("result");
    let host = host(&tmp);
    let answered = host
        .on_tool_result(&hub(), &a_call(), "ok", "the call's own output")
        .expect("the module answers `on_tool_result`");
    assert_eq!(
        answered.out.append.as_deref(),
        Some(" (the engine read this result)"),
        "the answer a guest hands back is the contract's own type: {:?}",
        answered.out
    );
    assert_eq!(answered.by, "s1");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **`notice`** (§4.3): the harness's sentence is reworded, resolved per KIND — `notice_job`, which
/// is the export this module has and the only notice it answers.
#[test]
fn a_notice_is_reworded_by_the_committed_module() {
    let tmp = tempdir("notice");
    let host = host(&tmp);
    assert_eq!(
        host.notice(
            &hub(),
            "job",
            "the harness's own sentence",
            json!({"handle": "j1"})
        ),
        Some("the engine reworded this notice".to_string())
    );
    // The kind nobody exports is the harness's own sentence, unchanged: a hook is resolved per
    // kind, so this module is not the answerer for the queue's verdicts.
    assert_eq!(
        host.notice(&hub(), "merge_queue", "the queue's own words", json!({})),
        Some("the queue's own words".to_string())
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **`condition`** (§4.4): the module arms a watch through the daemon's seam, its look runs under
/// the `read` capability it declared, and its own interval and state come back through the typed
/// `ConditionOut`.
#[test]
fn a_watch_is_armed_by_the_committed_module() {
    let tmp = tempdir("condition");
    std::fs::write(tmp.join("workspace/status.json"), "{\"armed\":true}\n").expect("a status file");
    let host = host(&tmp);
    let conditions = HostConditions::new(
        Arc::new(Mutex::new(Some(host.clone()))),
        hub(),
        // No gate: this look is a `read`, which is not the gate's question.
        None,
    );
    let armed = conditions
        .arm("engine", json!({"pr": "o/r#24"}))
        .expect("the module answers `condition_engine`");
    assert!(
        armed.condition.describe().contains("plugin `engine`"),
        "the watch is the module's own: {}",
        armed.condition.describe()
    );
    // Sixty seconds is the module's own `every_s`, and the cap it did not name is the tree's week.
    let seen = host.recorded("condition_engine");
    assert_eq!(
        seen[0]["args"],
        json!({"pr": "o/r#24"}),
        "arming carries the call's args"
    );
    assert!(seen[0]["look"].is_null(), "arming asks with no look");
    assert_eq!(
        armed.condition.met(),
        None,
        "the module declared no `fired`, so nothing fires — and the read look it asked for ran"
    );
    let after = host.recorded("condition_engine");
    let handed = after
        .iter()
        .find(|i| !i["look"].is_null())
        .unwrap_or_else(|| panic!("the look's result was handed back to the module: {after:?}"));
    assert_eq!(
        handed["look"]["stdout"], "{\"armed\":true}\n",
        "a `read` look ran under the layer's capability: {handed}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
