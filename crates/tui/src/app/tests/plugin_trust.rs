//! **D17's ladder, from the operator's own seat** — `/plugin` and `/plugin trust NAME`.
//!
//! The operator, 2026-10-31: *"Only the builtins run freely; anything I place later needs a pin."*
//! The ladder itself is `letibot_plugins::Trust` and it is enforced where a layer reaches the
//! world (`crates/harnessd/src/plugins/{tool,conditions}.rs`). What is here is the ONE act that
//! puts a placed layer on the free side of it, and the listing that says which side each one is
//! on — the door a person has, and not one a model has: the `plugin` tool has no `trust` verb,
//! because a layer that could pin itself would run freely because it said so.

use super::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A loader that answers for a layer's module with one that exports nothing, so a manifest with
/// `hooks = []` admits and a manifest with hooks does not — which is all this test needs of an
/// engine. The built-ins go to the real one, the way the head's own stack does it.
struct NoHooks;

impl letibot_plugins::Module for NoHooks {
    fn hooks(&self) -> Vec<String> {
        Vec::new()
    }

    fn call(&self, _hook: &str, _input: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn fuel(&self) -> u64 {
        0
    }
}

struct Layers;

impl letibot_plugins::Loader for Layers {
    fn load(
        &self,
        _wasm: &Path,
        _caps: &[letibot_plugins::Capability],
    ) -> Result<Box<dyn letibot_plugins::Module>, String> {
        Ok(Box::new(NoHooks))
    }

    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[letibot_plugins::Capability],
    ) -> Result<Box<dyn letibot_plugins::Module>, String> {
        crate::app::sessions_pane::RanoLoader.load_bytes(name, bytes, caps)
    }
}

/// A project with one placed layer, and the trust book beside it — a temp directory, so nothing
/// in this file can reach the operator's own `~/.config/letibot/plugin-trust.json`.
fn a_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-plugin-trust-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let layer = dir.join(".letibot").join("plugins").join("one");
    std::fs::create_dir_all(&layer).expect("the layer's directory");
    std::fs::write(
        layer.join("plugin.toml"),
        "name = \"one\"\npurpose = \"a layer a person placed\"\nhooks = []\n\
         capabilities = []\ncontract = 1\n",
    )
    .expect("the manifest");
    std::fs::write(layer.join("plugin.wasm"), b"\0asm").expect("a module");
    dir
}

/// A head whose plugin stack is this project's, with this project's trust book.
fn a_head_over(project: &Path) -> (App, Arc<letibot_plugins::Trust>) {
    let trust = letibot_plugins::Trust::open(project.join("plugin-trust.json"));
    let mut stack = letibot_plugins::Stack::discover(None, Some(project), None, Arc::new(Layers));
    stack.set_trust(trust.clone());
    let mut a = app();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    (a, trust)
}

/// **`/plugin trust NAME` pins what is on disk and says which hash it pinned.**
///
/// Three facts, and each is a clause of the requirement: the pin is recorded (the book has it),
/// the answer names the hash, and the layer this head is holding is FREE from that moment — the
/// same stack re-decided, not a claim about a file somebody has to go and read.
#[test]
fn plugin_trust_pins_what_is_on_disk_and_reports_the_hash() {
    let project = a_project("pin");
    let (mut a, trust) = a_head_over(&project);
    // Before: a placed layer, and nothing has said these bytes are the person's.
    {
        let host = a.plugin_host();
        let placed = host.as_ref().expect("a stack").placed();
        assert_eq!(placed.len(), 1, "{placed:?}");
        assert!(!placed[0].trusted, "a placed layer needs a pin");
    }

    let action = a.command("plugin trust one");
    assert!(action.is_none(), "a head verb, not one for the daemon");
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("pinned `one`"), "{said}");
    assert!(said.contains("module sha256"), "{said}");
    assert!(said.contains("manifest sha256"), "{said}");

    // **The pin is what the book holds, and it is about the bytes on disk now.**
    let pins = trust.pins();
    assert_eq!(pins.len(), 1, "{pins:?}");
    let module = letibot_plugins::Trust::open(project.join("plugin-trust.json")).pins()[0]
        .module
        .clone();
    assert!(!module.is_empty(), "the module's hash was recorded");
    assert!(said.contains(&module), "the answer names it: {said}");
    // And the head's own stack agrees at once, without waiting for a poll.
    {
        let host = a.plugin_host();
        let placed = host.as_ref().expect("a stack").placed();
        assert!(placed[0].trusted, "the pin took in this head's stack");
        assert_eq!(
            placed[0].pin.as_ref().map(|p| p.module.clone()),
            Some(module)
        );
    }
}

/// **`/plugin` lists the layers a person placed, and says which are pinned.**
#[test]
fn plugin_lists_the_placed_layers_and_their_standing() {
    let project = a_project("list");
    let (mut a, _trust) = a_head_over(&project);
    let listed = a.plugin_trust_listing().join("\n");
    assert!(listed.contains("one"), "{listed}");
    assert!(listed.contains("not pinned"), "{listed}");
    assert!(
        listed.contains("/plugin trust one"),
        "the spelling that pins it: {listed}"
    );

    a.command("plugin trust one");
    let listed = a.plugin_trust_listing().join("\n");
    assert!(listed.contains("pinned"), "{listed}");
    assert!(!listed.contains("not pinned"), "{listed}");
}

/// **A built-in needs no pin, and the door says so rather than pinning it.** The operator's rule
/// is *"only the builtins run freely"*: a built-in is the binary's own bytes, so a `/plugin
/// trust` over one would be a record that changes nothing.
#[test]
fn plugin_trust_refuses_a_builtin_by_name() {
    let project = a_project("builtin");
    let trust = letibot_plugins::Trust::open(project.join("plugin-trust.json"));
    let mut stack = letibot_plugins::Stack::discover(None, Some(&project), None, Arc::new(Layers));
    stack.set_trust(trust.clone());
    // The binary's own layers, as the head's real stack carries them — the built-in `sessions`
    // layer is the one every head has.
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "{failed:?}");
    let mut a = app();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));

    let said = a.trust_plugin("trust sessions");
    assert!(
        said.contains("built-in"),
        "a built-in is not pinnable: {said}"
    );
    assert!(trust.pins().is_empty(), "nothing was recorded: {said}");
}
