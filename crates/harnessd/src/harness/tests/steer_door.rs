//! K4: the turn-end door. A `steer_` layer's answer replaces the harness's own decision, and two
//! rules stay the host's - the continuation budget, and an interrupted turn.

use super::*;
use letibot_plugin::steer::{After, Decision, Here};
use letibot_plugins::{BuildError, Builder, Capability, Loader, Module};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// A layer that answers `steer_turn` with a function of the input: the rules below read the facts,
/// which a script keyed by call number could not.
struct Rule(Arc<dyn Fn(&Value) -> Value + Send + Sync>);

impl Module for Rule {
    fn hooks(&self) -> Vec<String> {
        vec![letibot_plugin::steer::HOOK.to_string()]
    }

    fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
        let input: Value = serde_json::from_str(input).map_err(|e| format!("not an input: {e}"))?;
        Ok(Some((self.0)(&input).to_string()))
    }

    fn fuel(&self) -> u64 {
        5
    }
}

struct RuleLoader(Arc<dyn Fn(&Value) -> Value + Send + Sync>);

impl Loader for RuleLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        Ok(Box::new(Rule(self.0.clone())))
    }
}

/// Nothing is built here: the layer is written, not compiled.
struct NoBuild;

impl Builder for NoBuild {
    fn build(&self, dir: &Path) -> Result<String, BuildError> {
        Ok(format!("not built: {}", dir.display()))
    }
}

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A host whose one project layer answers `steer_turn` with `rule`.
fn steering_host(
    tag: &str,
    rule: Arc<dyn Fn(&Value) -> Value + Send + Sync>,
) -> (Scratch, Arc<crate::plugins::PluginHost>) {
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let layer = dir.join(".letibot/plugins/turn-end");
    std::fs::create_dir_all(&layer).expect("the layer directory");
    std::fs::write(
        layer.join("plugin.toml"),
        "name = \"turn-end\"\npurpose = \"decides a turn's end\"\n\
         hooks = [\"steer_turn\"]\ncapabilities = []\ncontract = 1\n",
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
    let host = crate::plugins::PluginHost::discover(
        "s-steer",
        &dir,
        None,
        None,
        Arc::new(RuleLoader(rule)),
        Arc::new(NoBuild),
    );
    (Scratch(dir), Arc::new(host))
}

/// Whether the session's log carries a refused layer answer, which is how a rule the host will not
/// take reaches a person.
fn refused_on_the_log(hub: &Arc<Hub>) -> bool {
    hub.retained().iter().any(|e| {
        matches!(&e.event, letibot_sessionlog::SessionEvent::Warning { code, .. } if code == "plugin_layer")
    })
}

/// A harness over a fresh scratch directory, or `None` when this box has no vocabulary GGUF.
fn a_harness(id: &str) -> Option<Harness> {
    let gguf = crate::vocab_gguf()?;
    let dir =
        letibot_sessionlog::testing::scratch_root().join(format!("{id}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    Some(Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens"))
}

/// K4's rule: the budget is the host's. A layer that spends what the turn has not got is refused,
/// the turn ends as the harness decided, and the refusal is on the session's log.
#[test]
fn a_continue_past_the_budget_is_refused() {
    let (_dir, host) = steering_host(
        "steer-budget",
        Arc::new(|_| json!({"continue": "one more round"})),
    );
    let hub = Hub::new("s-steer");
    let input = letibot_plugin::steer::Input::synthetic();
    let spent = Here {
        continues_left: 0,
        interrupted: false,
    };
    assert_eq!(host.steer(&hub, &input, &spent), None);
    assert!(refused_on_the_log(&hub), "{:?}", hub.retained());
    // The same answer with a continuation left is one.
    let left = Here {
        continues_left: 1,
        interrupted: false,
    };
    assert_eq!(
        host.steer(&hub, &input, &left),
        Some(Decision::Continue("one more round".into()))
    );
}

/// K4's other rule: an interrupted turn is the operator's stop, and the budget being left is not
/// the question - the answer is refused whatever it would have cost.
#[test]
fn an_interrupted_turn_is_never_continued() {
    let (_dir, host) = steering_host(
        "steer-interrupted",
        Arc::new(|_| json!({"continue": "one more round"})),
    );
    let hub = Hub::new("s-steer");
    let input = letibot_plugin::steer::Input::synthetic();
    let interrupted = Here {
        continues_left: 3,
        interrupted: true,
    };
    assert_eq!(host.steer(&hub, &input, &interrupted), None);
    assert!(refused_on_the_log(&hub), "{:?}", hub.retained());
}

/// G2's first item, as a project's own rule: a turn that began on a compaction's record goes on,
/// and every other turn ends as the harness decided. The fact it reads is the one the harness sets
/// on a fork, taken once so the turn after that is not marked.
#[test]
fn continue_after_compaction_is_a_steer_rule() {
    let rule = Arc::new(|input: &Value| {
        if input["after"] == json!("compaction") {
            json!({"continue": "continue from the summary above"})
        } else {
            json!({"stop": true})
        }
    });
    let (_dir, host) = steering_host("steer-after-a-fork", rule);
    let hub = Hub::new("s-steer");
    let here = Here {
        continues_left: 1,
        interrupted: false,
    };
    let mut input = letibot_plugin::steer::Input::synthetic();
    input.native = None;
    input.after = After::Turn;
    assert_eq!(host.steer(&hub, &input, &here), Some(Decision::Stop));
    input.after = After::Compaction;
    assert_eq!(
        host.steer(&hub, &input, &here),
        Some(Decision::Continue("continue from the summary above".into()))
    );
    // And the fact it read is the host's own: the flag a compaction's fork leaves, read once.
    if let Some(mut h) = a_harness("s-steer-after-a-fork") {
        assert_eq!(h.after_the_fold(), After::Turn, "nothing has folded yet");
        h.folded = true;
        assert_eq!(h.after_the_fold(), After::Compaction);
        assert_eq!(
            h.after_the_fold(),
            After::Turn,
            "taken, so the turn after the next one is not one after a compaction"
        );
    }
}
