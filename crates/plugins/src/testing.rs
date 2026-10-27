//! Fakes, so the whole of §2.1–§2.4 is testable without a wasm engine, a cargo, or a network.
//!
//! A stand-in `plugin.wasm` here is a **JSON script**, not a module: [`FakeLoader`] reads it and
//! produces a [`Fake`], a module whose answers come from a table. That is the point of the
//! `Loader` seam — a module is anything that can report its hooks and answer a hook with text, so
//! every question this crate's tests ask (precedence, pass, mask, a look below, a trap, a replay)
//! is a question about the stack, not about compiling wasm.
//!
//! Nothing here is compiled into the library: the module is `#[cfg(test)]`, so rano stays the only
//! engine in the tree and this crate's dependency list stays at three.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::manifest::Capability;
use crate::{BuildError, Builder, Loader, MANIFEST, Module, WASM};

/// What a fake module does for one hook.
#[derive(Clone, Debug)]
pub(crate) enum Act {
    /// Answer this JSON.
    Answer(Value),
    /// Answer `{"pass":true}` — the hook falls through, spelled in the answer rather than left out.
    Pass,
    /// Answer `Ok(None)` — the hook is unchanged, which is the other spelling of falling through.
    Unchanged,
    /// Answer `{"mask":true}` — no answer here and none below.
    Mask,
    /// Fail, as a trap or an exhausted fuel budget does.
    Trap(String),
    /// Answer text that is not JSON.
    Malformed(String),
    /// Ask the layer below on the first call, then answer with what came back.
    AskBelow(Value),
    /// Ask the layer below on every call, including the one that already carries `below` — the
    /// second ask, which must be a recorded failure and not a loop.
    AlwaysAsk(Value),
}

impl Act {
    pub(crate) fn answer(answer: Value) -> Act {
        Act::Answer(answer)
    }

    pub(crate) fn mask() -> Act {
        Act::Mask
    }

    pub(crate) fn pass() -> Act {
        Act::Pass
    }

    pub(crate) fn unchanged() -> Act {
        Act::Unchanged
    }

    pub(crate) fn trap(why: &str) -> Act {
        Act::Trap(why.to_string())
    }

    pub(crate) fn malformed(text: &str) -> Act {
        Act::Malformed(text.to_string())
    }

    pub(crate) fn ask_below(input: Value) -> Act {
        Act::AskBelow(input)
    }

    pub(crate) fn always_ask(input: Value) -> Act {
        Act::AlwaysAsk(input)
    }
}

/// A module that answers from a table.
pub(crate) struct Fake {
    hooks: Vec<String>,
    acts: BTreeMap<String, Act>,
    fuel: u64,
}

impl Fake {
    pub(crate) fn new(acts: Vec<(&str, Act)>) -> Fake {
        Fake {
            hooks: acts.iter().map(|(h, _)| (*h).to_string()).collect(),
            acts: acts.into_iter().map(|(h, a)| (h.to_string(), a)).collect(),
            fuel: 7,
        }
    }

    pub(crate) fn one(hook: &str, act: Act) -> Fake {
        Fake::new(vec![(hook, act)])
    }
}

impl Module for Fake {
    fn hooks(&self) -> Vec<String> {
        self.hooks.clone()
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        let Some(act) = self.acts.get(hook) else {
            return Ok(None);
        };
        let input: Value = serde_json::from_str(input).unwrap_or(Value::Null);
        Ok(Some(match act {
            Act::Answer(answer) => answer.to_string(),
            Act::Pass => json!({"pass": true}).to_string(),
            Act::Unchanged => return Ok(None),
            Act::Mask => json!({"mask": true}).to_string(),
            Act::Trap(why) => return Err(why.clone()),
            Act::Malformed(text) => text.clone(),
            Act::AskBelow(below) => match input.get("below") {
                // The second call: report what arrived under `below` beside the original input,
                // so a test can see the whole round trip rather than a flag saying it happened.
                Some(seen) => {
                    let mut original = input.clone();
                    if let Some(object) = original.as_object_mut() {
                        object.remove("below");
                    }
                    json!({"below_seen": seen, "input": original}).to_string()
                }
                None => json!({"below": below}).to_string(),
            },
            Act::AlwaysAsk(below) => json!({"below": below}).to_string(),
        }))
    }

    fn fuel(&self) -> u64 {
        self.fuel
    }
}

/// The script a stand-in `plugin.wasm` holds.
pub(crate) fn script(acts: &[(&str, Act)]) -> Vec<u8> {
    let mut answers = serde_json::Map::new();
    for (hook, act) in acts {
        let encoded = match act {
            Act::Answer(a) => json!({ "answer": a }),
            Act::Pass => json!({ "pass": true }),
            Act::Unchanged => json!({ "unchanged": true }),
            Act::Mask => json!({ "mask": true }),
            Act::Trap(why) => json!({ "trap": why }),
            Act::Malformed(text) => json!({ "malformed": text }),
            Act::AskBelow(input) => json!({ "ask_below": input }),
            Act::AlwaysAsk(input) => json!({ "always_ask": input }),
        };
        answers.insert((*hook).to_string(), encoded);
    }
    let hooks: Vec<&str> = acts.iter().map(|(h, _)| *h).collect();
    json!({"hooks": hooks, "answers": answers, "fuel": 7})
        .to_string()
        .into_bytes()
}

fn parse_script(bytes: &[u8]) -> Result<Fake, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("not a fake module: {e}"))?;
    let script: Value =
        serde_json::from_str(text).map_err(|e| format!("not a fake module: {e}"))?;
    let hooks: Vec<String> = script
        .get("hooks")
        .and_then(Value::as_array)
        .ok_or("not a fake module: no hooks")?
        .iter()
        .filter_map(|h| h.as_str().map(str::to_string))
        .collect();
    let mut acts = BTreeMap::new();
    for hook in &hooks {
        let encoded = &script["answers"][hook];
        let act = if let Some(a) = encoded.get("answer") {
            Act::Answer(a.clone())
        } else if encoded.get("pass").is_some() {
            Act::Pass
        } else if encoded.get("unchanged").is_some() {
            Act::Unchanged
        } else if encoded.get("mask").is_some() {
            Act::Mask
        } else if let Some(why) = encoded.get("trap") {
            Act::Trap(why.as_str().unwrap_or_default().to_string())
        } else if let Some(text) = encoded.get("malformed") {
            Act::Malformed(text.as_str().unwrap_or_default().to_string())
        } else if let Some(input) = encoded.get("ask_below") {
            Act::AskBelow(input.clone())
        } else if let Some(input) = encoded.get("always_ask") {
            Act::AlwaysAsk(input.clone())
        } else {
            return Err(format!("not a fake module: hook `{hook}` has no act"));
        };
        acts.insert(hook.clone(), act);
    }
    Ok(Fake {
        hooks,
        acts,
        fuel: script.get("fuel").and_then(Value::as_u64).unwrap_or(7),
    })
}

/// The `Loader` the tests use: a scripted `plugin.wasm` in, a [`Fake`] out.
///
/// It records what it was handed, so a test can assert that discovery passed a manifest's
/// capabilities to the engine — the list the engine grants imports from.
pub(crate) struct FakeLoader {
    failing: Option<String>,
    loaded: Mutex<Vec<(PathBuf, Vec<Capability>)>>,
}

impl FakeLoader {
    pub(crate) fn new() -> FakeLoader {
        FakeLoader {
            failing: None,
            loaded: Mutex::new(Vec::new()),
        }
    }

    /// A loader that refuses everything, as an engine that cannot compile the module would.
    pub(crate) fn failing(why: &str) -> FakeLoader {
        FakeLoader {
            failing: Some(why.to_string()),
            loaded: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn caps_of(&self, wasm: &Path) -> Option<Vec<Capability>> {
        let loaded = self.loaded.lock().ok()?;
        loaded
            .iter()
            .rev()
            .find(|(p, _)| p == wasm)
            .map(|(_, caps)| caps.clone())
    }
}

impl Default for FakeLoader {
    fn default() -> FakeLoader {
        FakeLoader::new()
    }
}

impl Loader for FakeLoader {
    fn load(&self, wasm: &Path, caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        if let Ok(mut loaded) = self.loaded.lock() {
            loaded.push((wasm.to_path_buf(), caps.to_vec()));
        }
        if let Some(why) = &self.failing {
            return Err(why.clone());
        }
        let bytes = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        Ok(Box::new(parse_script(&bytes)?))
    }

    /// The bytes path, over the same scripts — a built-in is a script with no file behind it.
    ///
    /// The name stands in for a path in the record, so a test can assert that the capabilities a
    /// built-in's manifest declares were handed to the engine like any other layer's.
    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        if let Ok(mut loaded) = self.loaded.lock() {
            loaded.push((PathBuf::from(format!("<{name}>")), caps.to_vec()));
        }
        if let Some(why) = &self.failing {
            return Err(why.clone());
        }
        Ok(Box::new(parse_script(bytes)?))
    }
}

/// A `Builder` that writes a prepared script instead of running cargo, so no test here builds
/// anything.
pub(crate) struct FakeBuilder {
    body: Vec<u8>,
    log: String,
    failing: Option<String>,
}

impl FakeBuilder {
    /// "Build" a layer whose module answers `acts`.
    pub(crate) fn writing(acts: &[(&str, Act)]) -> FakeBuilder {
        FakeBuilder {
            body: script(acts),
            log: "Finished `release` profile [optimized] target(s)\n".to_string(),
            failing: None,
        }
    }

    /// "Build" a layer into a `plugin.wasm` that is not a module at all.
    pub(crate) fn writing_bytes(body: &str) -> FakeBuilder {
        FakeBuilder {
            body: body.as_bytes().to_vec(),
            log: String::new(),
            failing: None,
        }
    }

    /// A build that fails, as a source error does, with the log a person would read.
    pub(crate) fn failing(why: &str) -> FakeBuilder {
        FakeBuilder {
            body: Vec::new(),
            log: "error[E0425]: cannot find value `plan` in this scope\n".to_string(),
            failing: Some(why.to_string()),
        }
    }
}

impl Builder for FakeBuilder {
    fn build(&self, dir: &Path) -> Result<String, BuildError> {
        if let Some(why) = &self.failing {
            return Err(BuildError {
                why: why.clone(),
                log: self.log.clone(),
            });
        }
        std::fs::write(dir.join(WASM), &self.body).map_err(|e| BuildError {
            why: format!("{}: {e}", dir.display()),
            log: self.log.clone(),
        })?;
        Ok(self.log.clone())
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A directory under `std::env::temp_dir()` with a name no other test in this process takes,
/// removed when the test ends — including when it ends by panicking.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(tag: &str) -> TempDir {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("letibot-plugins-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a directory under the temp dir");
        TempDir { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write a layer directory: `plugin.toml` declaring `hooks`, and a `plugin.wasm` script that
/// answers all of them with `act`.
pub(crate) fn layer(root: &Path, name: &str, hooks: &[&str], act: Act) {
    layer_with(root, name, hooks, &[], act);
}

/// The same, for a layer whose manifest asks for capabilities.
pub(crate) fn layer_with(root: &Path, name: &str, hooks: &[&str], capabilities: &[&str], act: Act) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("a layer directory");
    let hooks_toml: Vec<String> = hooks.iter().map(|h| format!("\"{h}\"")).collect();
    let caps_toml: Vec<String> = capabilities.iter().map(|c| format!("\"{c}\"")).collect();
    let manifest = format!(
        "name = \"{name}\"\npurpose = \"a layer the tests control\"\nhooks = [{}]\ncapabilities = [{}]\ncontract = 1\n",
        hooks_toml.join(", "),
        caps_toml.join(", ")
    );
    std::fs::write(dir.join(MANIFEST), manifest).expect("plugin.toml");
    write_wasm(&dir, hooks, act);
}

/// Rewrite a layer's `plugin.wasm` — a rebuild, or a change to what it answers.
pub(crate) fn write_wasm(dir: &Path, hooks: &[&str], act: Act) {
    let acts: Vec<(&str, Act)> = hooks.iter().map(|h| (*h, act.clone())).collect();
    write_wasm_bytes(dir, &script(&acts));
}

/// Write a layer's `plugin.wasm` byte for byte — a hand-edited file, or a rebuild whose bytes
/// moved without its behaviour moving.
pub(crate) fn write_wasm_bytes(dir: &Path, bytes: &[u8]) {
    std::fs::write(dir.join(WASM), bytes).expect("plugin.wasm");
}
