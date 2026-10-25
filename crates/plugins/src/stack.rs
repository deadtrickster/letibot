//! The stack: what layers exist, which one answers a hook, and what a change to one costs.
//!
//! # Discovery, and why the order is what it is
//!
//! Four scopes, in precedence: **session over project over user over built-in** (`§2.1`). Within
//! a scope the order is *creation order, newest on top*, so the stack reads as the history of the
//! fixes made in a session — the last layer written is the one whose answer wins. A layer
//! directory's mtime is its creation order here, with the name as the tie-break, because a
//! directory has no birth time in `std` and two layers added in the same second are still
//! ordered, just by name.
//!
//! Ids follow from that order and never from a file: `s1`, `s2`, `p1`, `u1`. Built-ins are named
//! (`b-github-pr`) because they are a fixed set the host compiled in, not history.
//!
//! A directory with no readable `plugin.toml` is **not a layer**: it is a [`Failure`] naming the
//! directory. A layer whose manifest parses but whose module will not load *is* a layer, listed
//! with its module missing — the stack view shows it broken instead of showing nothing, which is
//! the difference between "this fix is not installed" and "this fix is installed and exploding".
//!
//! # Failures during a call are the call's, not the stack's
//!
//! [`Stack::call`] takes `&self`, because a stack is shared: the daemon keeps one behind an
//! `Arc` and every hook call in every turn goes through it. So a failure met *during* a
//! resolution is returned in [`Resolved::failures`], where the caller that must tell the model
//! has it in hand, and [`Stack::failures`] is the other kind — what discovery, `apply` and
//! `poll_changes` could not load, which is a property of the stack rather than of one call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::Value;

use crate::manifest::{Manifest, Scope};
use crate::{Loader, MANIFEST, Module, WASM, sha256_file};

/// How many module calls one resolution may make.
///
/// A look nests — a layer asks below, that layer asks below — so a stack of ten layers could in
/// principle ask for a great many answers to settle one hook. The budget turns a pathological
/// arrangement into a recorded failure with an answer from below, instead of a pane that takes a
/// second to draw. Ten layers of one look each need far fewer than this.
const MAX_CALLS: usize = 32;

/// A layer: where it came from, what it declares, and which bytes it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// `s3`, `p1`, `u1` — or `b-github-pr` for a built-in.
    pub id: String,
    pub scope: Scope,
    /// The layer's directory. Empty for a built-in, which has none.
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// The SHA-256 of `plugin.wasm` — or empty when there is no module to hash.
    pub sha256: String,
    pub disabled: bool,
}

/// One layer's refusal to answer, and why. Rendered for the model and for the stack pane, so the
/// display is part of the type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub layer: String,
    pub scope: Scope,
    /// The hook being resolved, or none when the failure is the layer itself (a load).
    pub hook: Option<String>,
    pub why: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): ", self.layer, self.scope)?;
        if let Some(hook) = &self.hook {
            write!(f, "`{hook}`: ")?;
        }
        f.write_str(&self.why)
    }
}

/// What one hook resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// The answer, or none when nothing answered or a layer masked the hook.
    pub answer: Option<Value>,
    /// The layer whose answer it is — the topmost one, since a layer that looks below still
    /// answers for itself.
    pub by: Option<String>,
    pub failures: Vec<Failure>,
}

/// A layer's module changed on disk and is waiting for the gate.
///
/// The gate fills in the module it loaded, and [`Stack::apply`] swaps in **exactly that one**:
/// applying a second compile of the same bytes would mean the module that answers is not the
/// module that was replayed, which is the one thing the gate exists to prevent.
pub struct Candidate {
    /// The id of the layer this replaces, or empty for a layer that does not exist yet.
    pub layer: String,
    pub scope: Scope,
    pub dir: PathBuf,
    /// The manifest read when the change was noticed.
    pub manifest: Manifest,
    /// The `plugin.wasm` the poll saw — before the gate's build rewrote it.
    pub sha256: String,
    replayed: Mutex<Option<(String, Arc<dyn Module>)>>,
}

impl Candidate {
    fn new(
        layer: String,
        scope: Scope,
        dir: PathBuf,
        manifest: Manifest,
        sha256: String,
    ) -> Candidate {
        Candidate {
            layer,
            scope,
            dir,
            manifest,
            sha256,
            replayed: Mutex::new(None),
        }
    }

    /// Keep the module the gate loaded and the bytes it loaded from.
    pub(crate) fn remember(&self, sha: &str, module: Arc<dyn Module>) {
        if let Ok(mut slot) = self.replayed.lock() {
            *slot = Some((sha.to_string(), module));
        }
    }

    /// The replayed module, when these are the bytes that were replayed.
    pub(crate) fn replayed(&self, sha: &str) -> Option<Arc<dyn Module>> {
        match self.replayed.lock() {
            Ok(slot) => match slot.as_ref() {
                Some((seen, module)) if seen == sha => Some(module.clone()),
                _ => None,
            },
            Err(_) => None,
        }
    }

    /// Whether a gate has run over this candidate at all — which is what tells "the file moved
    /// after the gate" apart from "nothing was gated here".
    fn was_replayed(&self) -> bool {
        matches!(self.replayed.lock(), Ok(slot) if slot.is_some())
    }

    /// Whether a gate has run over this candidate at all — the public half of the check
    /// [`Stack::apply`] makes, for a caller that wants to report it.
    pub fn gated(&self) -> bool {
        self.was_replayed()
    }
}

impl Clone for Candidate {
    fn clone(&self) -> Candidate {
        Candidate {
            layer: self.layer.clone(),
            scope: self.scope,
            dir: self.dir.clone(),
            manifest: self.manifest.clone(),
            sha256: self.sha256.clone(),
            replayed: Mutex::new(self.replayed.lock().ok().and_then(|s| s.clone())),
        }
    }
}

/// What a layer's `plugin.wasm` looked like the last time the stack read it.
///
/// The SHA-256 is the truth — it is what a layer *is*, in the transcript and in the stack view.
/// The mtime and the length are the cheap half of a poll: a file that has not moved in either
/// cannot have different bytes, so a poll over a quiet stack stats files and hashes none of them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    mtime: Option<SystemTime>,
    len: u64,
    sha256: String,
}

impl Fingerprint {
    fn absent() -> Fingerprint {
        Fingerprint {
            mtime: None,
            len: 0,
            sha256: String::new(),
        }
    }
}

fn fingerprint(path: &Path) -> Result<Fingerprint, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Fingerprint {
        mtime: meta.modified().ok(),
        len: meta.len(),
        sha256: sha256_file(path)?,
    })
}

/// Everything about a layer that is not in the [`Layer`] itself: its module, the hooks that
/// module exports, and the bytes it was read from.
#[derive(Clone)]
struct Wired {
    /// `None` when the module could not be loaded or admitted; the reason is in the stack's
    /// failures and the layer still appears in the stack view.
    module: Option<Arc<dyn Module>>,
    hooks: Vec<String>,
    seen: Fingerprint,
}

/// The four scopes resolved into one stack.
///
/// Layered as the module doc says, and cloneable so the gate can fork one: the fork is the
/// shadow of §2.4, and the live stack it was cloned from is untouched.
#[derive(Clone)]
pub struct Stack {
    /// Newest-first within scope, scopes in precedence order. This vector *is* the order a hook
    /// is resolved in. The modules live beside it, keyed by id, so cloning a stack shares them
    /// (an `Arc` each) and no two orderings can drift apart.
    layers: Vec<Layer>,
    wired: BTreeMap<String, Wired>,
    loader: Arc<dyn Loader>,
    failures: Vec<Failure>,
}

impl Stack {
    /// Discover layers from the four scopes.
    ///
    /// Each argument is the *root of a scope* and may be absent, which is an empty scope and not
    /// an error — a head with no project, a user who has installed nothing:
    ///
    /// | scope | where the layer directories are |
    /// |---|---|
    /// | session | under `session_dir` itself (the session's own plugin directory) |
    /// | project | under `<project>/.letibot/plugins` |
    /// | user | under `<user>/plugins`, with `<user>` the letibot config dir (`~/.config/letibot`) |
    ///
    /// Built-ins are added afterwards with [`Stack::register_builtin`], because they are compiled
    /// into the host rather than found on disk.
    pub fn discover(
        session_dir: Option<&Path>,
        project: Option<&Path>,
        user: Option<&Path>,
        loader: Arc<dyn Loader>,
    ) -> Stack {
        let mut stack = Stack {
            layers: Vec::new(),
            wired: BTreeMap::new(),
            loader,
            failures: Vec::new(),
        };
        if let Some(dir) = session_dir {
            stack.discover_scope(Scope::Session, dir);
        }
        if let Some(root) = project {
            stack.discover_scope(Scope::Project, &root.join(".letibot").join("plugins"));
        }
        if let Some(root) = user {
            stack.discover_scope(Scope::User, &root.join("plugins"));
        }
        stack
    }

    /// Every layer, in the order a hook is resolved through them.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// The engine this stack loads its modules with — the gate's step 2 and `apply`'s ungated
    /// path both go through the stack's own loader, so a shadow is compiled by the same engine as
    /// the live stack.
    pub(crate) fn loader(&self) -> &Arc<dyn Loader> {
        &self.loader
    }

    /// What discovery, `apply` and `poll_changes` could not load. A failure met while resolving a
    /// hook is in [`Resolved::failures`] instead — see the module doc.
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }

    /// One layer by id.
    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// Register a built-in default: a layer the host compiled in, with the module that answers
    /// for it. The **last registered is on top** of the built-in scope, which is the same
    /// newest-on-top rule the disk scopes follow — so register the least specific default first.
    ///
    /// An empty id is filled in as `b-<name>`. A layer whose manifest and exports disagree, or
    /// whose id is already taken, is refused and recorded rather than registered: ids are how
    /// `set_disabled`, `by` and the transcript name a layer, so two of them cannot be one.
    pub fn register_builtin(&mut self, mut layer: Layer, module: Box<dyn Module>) {
        if layer.id.is_empty() {
            layer.id = format!("{}-{}", Scope::BuiltIn.prefix(), layer.manifest.name);
        }
        layer.scope = Scope::BuiltIn;
        if self.layers.iter().any(|l| l.id == layer.id) {
            self.failures.push(Failure {
                layer: layer.id.clone(),
                scope: Scope::BuiltIn,
                hook: None,
                why: "a built-in with this id is already registered".to_string(),
            });
            return;
        }
        let module: Arc<dyn Module> = Arc::from(module);
        let hooks = module.hooks();
        if let Err(why) = layer.manifest.admit(&hooks) {
            self.failures.push(Failure {
                layer: layer.id.clone(),
                scope: Scope::BuiltIn,
                hook: None,
                why,
            });
            return;
        }
        let id = layer.id.clone();
        let at = self.insertion_point(Scope::BuiltIn);
        self.layers.insert(at, layer);
        self.wired.insert(
            id,
            Wired {
                module: Some(module),
                hooks,
                seen: Fingerprint::absent(),
            },
        );
    }

    /// Turn a layer off (or on) without deleting it — `plugin disable s2`, the bisect for
    /// *"which of my fixes broke this"*. A disabled layer stays in the stack view, greyed, which
    /// is the point: the stack reads as history, and history that vanished cannot be read.
    pub fn set_disabled(&mut self, id: &str, disabled: bool) -> Result<(), String> {
        let i = self.layers.iter().position(|l| l.id == id).ok_or_else(|| {
            let ids: Vec<&str> = self.layers.iter().map(|l| l.id.as_str()).collect();
            format!("no layer `{id}` — the stack has [{}]", ids.join(", "))
        })?;
        self.layers[i].disabled = disabled;
        Ok(())
    }

    /// Resolve one hook through the stack, from the top.
    ///
    /// The protocol is the crate doc's one table: pass or unchanged falls through, mask stops
    /// everything below, a look below round trips, anything else answers. A layer that fails is
    /// recorded and skipped, and **one failure never stops the resolution** — the layer below
    /// still answers, which is what keeps a broken fix from taking the session down with it.
    pub fn call(&self, hook: &str, input: &Value) -> Resolved {
        let mut walk = Walk {
            hook,
            failures: Vec::new(),
            calls: 0,
            spent: false,
        };
        let (answer, by) = match self.run(0, input, &mut walk) {
            Step::Answer { value, by } => (Some(value), Some(by)),
            Step::Passed | Step::Masked => (None, None),
        };
        Resolved {
            answer,
            by,
            failures: walk.failures,
        }
    }

    /// Re-stat each layer's `plugin.wasm`; the ones whose bytes changed come back as candidates.
    ///
    /// A change is pending until it is applied, so polling again reports it again: a candidate
    /// the gate refused has not happened, and a caller that saw one and did nothing about it gets
    /// told again. A layer with no `plugin.wasm` at all is silent — that is a build in progress,
    /// and a failure recorded every poll would be noise, not news.
    pub fn poll_changes(&mut self) -> Vec<Candidate> {
        let mut out = Vec::new();
        for i in 0..self.layers.len() {
            let layer = &self.layers[i];
            if layer.scope == Scope::BuiltIn || layer.dir.as_os_str().is_empty() {
                continue;
            }
            let wasm = layer.dir.join(WASM);
            let Ok(meta) = std::fs::metadata(&wasm) else {
                continue;
            };
            let known = self
                .wired
                .get(&layer.id)
                .map(|w| w.seen.clone())
                .unwrap_or_else(Fingerprint::absent);
            if meta.modified().ok() == known.mtime && meta.len() == known.len {
                continue;
            }
            let seen = match fingerprint(&wasm) {
                Ok(seen) => seen,
                Err(why) => {
                    self.failures.push(Failure {
                        layer: layer.id.clone(),
                        scope: layer.scope,
                        hook: None,
                        why,
                    });
                    continue;
                }
            };
            if seen.sha256 == known.sha256 {
                // A touch, not a change: remember that the file moved and report nothing.
                if let Some(wired) = self.wired.get_mut(&layer.id) {
                    wired.seen = seen;
                }
                continue;
            }
            match Manifest::read(&layer.dir) {
                Ok(manifest) => out.push(Candidate::new(
                    layer.id.clone(),
                    layer.scope,
                    layer.dir.clone(),
                    manifest,
                    seen.sha256,
                )),
                Err(why) => self.failures.push(Failure {
                    layer: layer.id.clone(),
                    scope: layer.scope,
                    hook: None,
                    why,
                }),
            }
        }
        out
    }

    /// A candidate for a directory, whether it is a layer the stack has or one it has never seen
    /// (a new layer takes its scope from `scope` and gets its id when it is applied).
    pub fn candidate(&self, dir: &Path, scope: Scope) -> Result<Candidate, String> {
        let manifest = Manifest::read(dir)?;
        let sha256 = sha256_file(&dir.join(WASM))
            .map_err(|why| format!("{why} — a layer needs its module before it can be gated"))?;
        let id = self
            .layers
            .iter()
            .find(|l| l.dir == dir)
            .map(|l| l.id.clone())
            .unwrap_or_default();
        Ok(Candidate::new(
            id,
            scope,
            dir.to_path_buf(),
            manifest,
            sha256,
        ))
    }

    /// Put a candidate into the stack — **only after the gate passed**; this is the door the gate
    /// does not close for you.
    ///
    /// Two things are refused here that the gate cannot see: a `plugin.wasm` that is no longer
    /// the bytes the gate replayed (a build that ran between the two), because what answers must
    /// be what was replayed; and a manifest that no longer admits its module. Applying a
    /// candidate that was never gated loads what is on disk, which is what a caller who is not
    /// using the gate wants — the gate is the path §2.3 requires, not a thing this method can
    /// check.
    pub fn apply(&mut self, c: Candidate) -> Result<(), String> {
        if c.scope == Scope::BuiltIn {
            return Err(
                "a built-in has no directory to reload — register_builtin is that door".to_string(),
            );
        }
        let dir = c.dir.clone();
        let manifest = Manifest::read(&dir)?;
        let wasm = dir.join(WASM);
        let seen =
            fingerprint(&wasm).map_err(|why| format!("{why} — apply needs the built module"))?;
        let module = match c.replayed(&seen.sha256) {
            Some(module) => module,
            None if c.was_replayed() => {
                return Err(format!(
                    "{}: plugin.wasm is not the bytes the gate replayed — gate it again",
                    dir.display()
                ));
            }
            None => Arc::from(self.loader.load(&wasm, &manifest.capabilities)?),
        };
        let hooks = module.hooks();
        manifest
            .admit(&hooks)
            .map_err(|why| format!("{}: {why}", dir.display()))?;
        let wired = Wired {
            module: Some(module),
            hooks,
            seen: seen.clone(),
        };
        match self.position(&dir) {
            Some(i) => {
                let id = self.layers[i].id.clone();
                self.layers[i].manifest = manifest;
                self.layers[i].sha256 = seen.sha256;
                self.wired.insert(id, wired);
            }
            None => {
                let id = self.next_id(c.scope);
                let layer = Layer {
                    id: id.clone(),
                    scope: c.scope,
                    dir,
                    manifest,
                    sha256: seen.sha256,
                    disabled: false,
                };
                let at = self.insertion_point(c.scope);
                self.layers.insert(at, layer);
                self.wired.insert(id, wired);
            }
        }
        Ok(())
    }

    fn position(&self, dir: &Path) -> Option<usize> {
        self.layers
            .iter()
            .position(|l| l.scope != Scope::BuiltIn && l.dir == dir)
    }

    /// The top of a scope: before the first layer that is this scope or a lower-precedence one.
    /// A new layer lands there, which is the newest-on-top rule with an id it has not earned yet.
    fn insertion_point(&self, scope: Scope) -> usize {
        self.layers
            .iter()
            .position(|l| l.scope.rank() >= scope.rank())
            .unwrap_or(self.layers.len())
    }

    fn next_id(&self, scope: Scope) -> String {
        let n = self.layers.iter().filter(|l| l.scope == scope).count() + 1;
        format!("{}{n}", scope.prefix())
    }

    fn discover_scope(&mut self, scope: Scope, dir: &Path) {
        let dirs = layer_dirs(dir);
        let mut found = Vec::new();
        for path in dirs {
            match Manifest::read(&path) {
                Ok(manifest) => found.push((path, manifest)),
                Err(why) => self.failures.push(Failure {
                    layer: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string()),
                    scope,
                    hook: None,
                    why,
                }),
            }
        }
        // Newest first on the stack, but the ids count from the oldest: `s1` is the first layer
        // the session ever added, and its number must not move when a new one lands on top.
        for (n, (path, manifest)) in found.into_iter().enumerate().rev() {
            let id = format!("{}{}", scope.prefix(), n + 1);
            let wasm = path.join(WASM);
            let seen = match fingerprint(&wasm) {
                Ok(seen) => seen,
                Err(why) => {
                    self.failures.push(Failure {
                        layer: id.clone(),
                        scope,
                        hook: None,
                        why,
                    });
                    Fingerprint::absent()
                }
            };
            let module = match self.loader.load(&wasm, &manifest.capabilities) {
                Ok(module) => {
                    let hooks = module.hooks();
                    match manifest.admit(&hooks) {
                        Ok(()) => Some((Arc::from(module), hooks)),
                        Err(why) => {
                            self.failures.push(Failure {
                                layer: id.clone(),
                                scope,
                                hook: None,
                                why,
                            });
                            None
                        }
                    }
                }
                Err(why) => {
                    self.failures.push(Failure {
                        layer: id.clone(),
                        scope,
                        hook: None,
                        why,
                    });
                    None
                }
            };
            let (module, hooks) = match module {
                Some((module, hooks)) => (Some(module), hooks),
                None => (None, Vec::new()),
            };
            self.layers.push(Layer {
                id: id.clone(),
                scope,
                dir: path,
                sha256: seen.sha256.clone(),
                manifest,
                disabled: false,
            });
            self.wired.insert(
                id,
                Wired {
                    module,
                    hooks,
                    seen,
                },
            );
        }
    }

    fn run(&self, from: usize, input: &Value, walk: &mut Walk<'_>) -> Step {
        for i in from..self.layers.len() {
            if walk.spent {
                break;
            }
            let layer = &self.layers[i];
            if layer.disabled {
                continue;
            }
            let Some(wired) = self.wired.get(&layer.id) else {
                continue;
            };
            let Some(module) = wired.module.as_ref() else {
                continue;
            };
            if !wired.hooks.iter().any(|h| h == walk.hook) {
                continue;
            }
            match self.ask(i, module, input, false, walk) {
                Step::Passed => continue,
                other => return other,
            }
        }
        Step::Passed
    }

    /// One layer's answer, including the one look below it may take.
    fn ask(
        &self,
        i: usize,
        module: &Arc<dyn Module>,
        input: &Value,
        looked: bool,
        walk: &mut Walk<'_>,
    ) -> Step {
        let layer = &self.layers[i];
        if walk.spent {
            return Step::Passed;
        }
        if walk.calls >= MAX_CALLS {
            walk.spent = true;
            walk.fail(
                layer,
                format!(
                    "the resolution gave up after {MAX_CALLS} module calls — a stack that asks \
                     itself for answers in a loop"
                ),
            );
            return Step::Passed;
        }
        walk.calls += 1;
        let text = match module.call(walk.hook, &input.to_string()) {
            Ok(Some(text)) => text,
            Ok(None) => return Step::Passed,
            Err(why) => {
                walk.fail(layer, why);
                return Step::Passed;
            }
        };
        let Some(answer) = parse(&text, layer, walk) else {
            return Step::Passed;
        };
        if is(&answer, "mask") {
            return Step::Masked;
        }
        if let Some(look) = answer.get("look") {
            if looked {
                walk.fail(
                    layer,
                    "a second look inside one resolution — a layer may ask below once".to_string(),
                );
                return Step::Passed;
            }
            let Some(below_input) = look.get("below") else {
                walk.fail(
                    layer,
                    "`look` without `below` — there is nothing to ask the layer below".to_string(),
                );
                return Step::Passed;
            };
            // The layer below is resolved and its answer is handed back to this layer, which is
            // asked again with the same input plus `below`. A mask or a pass below is `null`:
            // the lower stack answered nothing, and the layer that asked is told so.
            let below = match self.run(i + 1, below_input, walk) {
                Step::Answer { value, .. } => value,
                Step::Passed | Step::Masked => Value::Null,
            };
            let Some(mut second) = input.as_object().cloned() else {
                walk.fail(
                    layer,
                    "asked below, but the hook's input is not an object, so `below` has nowhere to go"
                        .to_string(),
                );
                return Step::Passed;
            };
            second.insert("below".to_string(), below);
            let second = Value::Object(second);
            return self.ask(i, module, &second, true, walk);
        }
        if is(&answer, "pass") {
            return Step::Passed;
        }
        Step::Answer {
            value: answer,
            by: layer.id.clone(),
        }
    }
}

/// One layer's step in a resolution.
enum Step {
    Answer { value: Value, by: String },
    Passed,
    Masked,
}

/// The mutable half of one resolution: what it has recorded, and whether it still may ask.
struct Walk<'a> {
    hook: &'a str,
    failures: Vec<Failure>,
    calls: usize,
    spent: bool,
}

impl Walk<'_> {
    fn fail(&mut self, layer: &Layer, why: String) {
        self.failures.push(Failure {
            layer: layer.id.clone(),
            scope: layer.scope,
            hook: Some(self.hook.to_string()),
            why,
        });
    }
}

/// A layer's answer as JSON, or nothing plus a recorded failure. Malformed output is the one the
/// plan names explicitly (§1, principle 4): it is a bug in the plugin, and the layer below still
/// answers.
fn parse(text: &str, layer: &Layer, walk: &mut Walk<'_>) -> Option<Value> {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => Some(value),
        Err(e) => {
            let head: String = text.chars().take(120).collect();
            walk.fail(layer, format!("answer is not JSON ({e}): {head}"));
            None
        }
    }
}

fn is(answer: &Value, key: &str) -> bool {
    answer.get(key) == Some(&Value::Bool(true))
}

/// The layer directories under `dir`, oldest first: a child that holds a `plugin.toml`, ordered
/// by its mtime and then by its name.
fn layer_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(MANIFEST).is_file())
        .map(|p| {
            let mtime = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (mtime, name, p)
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, _, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;
    use crate::testing::{Act, Fake, TempDir, layer, write_wasm};
    use serde_json::json;

    fn stack(scopes: &TempDir) -> Stack {
        Stack::discover(
            Some(&scopes.path().join("session")),
            Some(&scopes.path().join("project")),
            Some(&scopes.path().join("user")),
            std::sync::Arc::new(crate::testing::FakeLoader::new()),
        )
    }

    fn answer_of(r: &Resolved) -> Value {
        r.answer.clone().expect("an answer")
    }

    #[test]
    fn precedence_walks_session_project_user_builtin() {
        let t = TempDir::new("precedence");
        for (root, name, mark) in [
            ("project/.letibot/plugins", "a", "project"),
            ("user/plugins", "a", "user"),
            ("session", "a", "session"),
        ] {
            layer(
                &t.path().join(root),
                name,
                &["pane_dashboard"],
                Act::answer(json!({"from": mark})),
            );
        }
        let mut s = stack(&t);
        assert_eq!(
            s.layers().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["s1", "p1", "u1"],
        );
        let r = s.call("pane_dashboard", &json!({"v": 1}));
        assert_eq!(answer_of(&r), json!({"from": "session"}));
        assert_eq!(r.by.as_deref(), Some("s1"));

        // The project is next, then the user, then nothing at all.
        s.set_disabled("s1", true).unwrap();
        assert_eq!(
            answer_of(&s.call("pane_dashboard", &json!({}))),
            json!({"from": "project"})
        );
        s.set_disabled("p1", true).unwrap();
        assert_eq!(
            answer_of(&s.call("pane_dashboard", &json!({}))),
            json!({"from": "user"})
        );
        s.set_disabled("u1", true).unwrap();
        assert_eq!(s.call("pane_dashboard", &json!({})).answer, None);
        assert!(s.failures().is_empty());
    }

    #[test]
    fn a_builtin_sits_below_every_scope_and_the_last_registered_is_on_top() {
        let t = TempDir::new("builtin");
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::answer(json!({"from": "session"})),
        );
        let mut s = stack(&t);
        for name in ["first", "second"] {
            let manifest = Manifest::parse(&format!(
                "name = \"{name}\"\npurpose = \"p\"\nhooks = [\"pane_dashboard\"]\ncapabilities = []\ncontract = 1\n"
            ))
            .unwrap();
            s.register_builtin(
                Layer {
                    id: String::new(),
                    scope: Scope::Session,
                    dir: PathBuf::new(),
                    manifest,
                    sha256: String::new(),
                    disabled: false,
                },
                Box::new(Fake::one(
                    "pane_dashboard",
                    Act::answer(json!({ "from": name })),
                )),
            );
        }
        assert_eq!(
            s.layers().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["s1", "b-second", "b-first"],
        );
        s.set_disabled("s1", true).unwrap();
        assert_eq!(
            answer_of(&s.call("pane_dashboard", &json!({}))),
            json!({"from": "second"})
        );
    }

    #[test]
    fn the_unit_of_override_is_a_hook_so_a_lower_layer_still_answers_the_others() {
        let t = TempDir::new("per-hook");
        layer(
            &t.path().join("session"),
            "a",
            &["condition_coderabbit"],
            Act::answer(json!({"condition": "session"})),
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["condition_coderabbit", "condition_github_pr"],
            Act::answer(json!({"condition": "project"})),
        );
        let s = stack(&t);
        assert_eq!(
            answer_of(&s.call("condition_coderabbit", &json!({}))),
            json!({"condition": "session"})
        );
        let r = s.call("condition_github_pr", &json!({}));
        assert_eq!(answer_of(&r), json!({"condition": "project"}));
        assert_eq!(r.by.as_deref(), Some("p1"));
        // A hook nobody exports has no answer and no failure: falling off the bottom is not an error.
        let r = s.call("notice_whatever", &json!({}));
        assert_eq!(r.answer, None);
        assert!(r.failures.is_empty());
    }

    #[test]
    fn pass_and_unchanged_both_fall_through() {
        let t = TempDir::new("pass");
        layer(&t.path().join("session"), "a", &["notice_fired"], Act::Pass);
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["notice_fired"],
            Act::unchanged(),
        );
        layer(
            &t.path().join("user/plugins"),
            "c",
            &["notice_fired"],
            Act::answer(json!({"text": "user"})),
        );
        let r = stack(&t).call("notice_fired", &json!({"v": 1}));
        assert_eq!(answer_of(&r), json!({"text": "user"}));
        assert_eq!(r.by.as_deref(), Some("u1"));
        assert!(r.failures.is_empty());
    }

    #[test]
    fn a_mask_switches_the_hook_off_from_that_layer_down() {
        let t = TempDir::new("mask");
        layer(
            &t.path().join("session"),
            "a",
            &["notice_fired"],
            Act::mask(),
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["notice_fired"],
            Act::answer(json!({"text": "project"})),
        );
        let r = stack(&t).call("notice_fired", &json!({"v": 1}));
        assert_eq!(r.answer, None);
        assert_eq!(r.by, None);
        assert!(r.failures.is_empty(), "a mask is an answer, not a failure");
    }

    #[test]
    fn a_look_below_round_trips_the_lower_answer_into_the_layer_that_asked() {
        let t = TempDir::new("look");
        // The session layer changes one field of whatever answers below: `{"look":{"below":…}}`
        // on the first call, and on the second it answers with what it was handed.
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::look_below(json!({"v": 1, "edited": true})),
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["pane_dashboard"],
            Act::answer(json!({"title": "below answered"})),
        );
        let r = stack(&t).call(
            "pane_dashboard",
            &json!({"v": 1, "size": {"w": 80, "h": 24}}),
        );
        assert_eq!(
            r.by.as_deref(),
            Some("s1"),
            "the layer that asked below is the one that answers"
        );
        assert_eq!(
            answer_of(&r),
            json!({"below_seen": {"title": "below answered"}, "input": {"v": 1, "size": {"w": 80, "h": 24}}}),
            "the below answer arrives under `below`, beside the original input",
        );
        assert!(r.failures.is_empty());
    }

    #[test]
    fn a_trapping_top_layer_falls_to_the_next_and_is_recorded() {
        let t = TempDir::new("trap");
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::trap("unreachable: memory access out of bounds"),
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["pane_dashboard"],
            Act::answer(json!({"from": "project"})),
        );
        let r = stack(&t).call("pane_dashboard", &json!({}));
        assert_eq!(answer_of(&r), json!({"from": "project"}));
        assert_eq!(r.by.as_deref(), Some("p1"));
        assert_eq!(r.failures.len(), 1);
        let f = &r.failures[0];
        assert_eq!(f.layer, "s1");
        assert_eq!(f.scope, Scope::Session);
        assert_eq!(f.hook.as_deref(), Some("pane_dashboard"));
        assert!(f.why.contains("out of bounds"), "{f}");
    }

    #[test]
    fn malformed_json_is_a_failure_and_the_layer_below_still_answers() {
        let t = TempDir::new("malformed");
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::malformed("{not json"),
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["pane_dashboard"],
            Act::answer(json!({"from": "project"})),
        );
        let r = stack(&t).call("pane_dashboard", &json!({}));
        assert_eq!(r.by.as_deref(), Some("p1"));
        assert!(r.failures[0].why.contains("not JSON"), "{}", r.failures[0]);
    }

    #[test]
    fn a_second_look_from_one_layer_is_a_recorded_failure_not_a_loop() {
        let t = TempDir::new("two-looks");
        // This fake answers `look below` on every call, including the one carrying `below`.
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::always_look(json!({"v": 1})),
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["pane_dashboard"],
            Act::answer(json!({"from": "project"})),
        );
        let r = stack(&t).call("pane_dashboard", &json!({}));
        assert_eq!(r.by.as_deref(), Some("p1"));
        assert!(
            r.failures.iter().any(|f| f.why.contains("second look")),
            "{:?}",
            r.failures
        );
    }

    #[test]
    fn a_disabled_layer_re_resolves_as_if_it_were_not_there() {
        let t = TempDir::new("disable");
        layer(
            &t.path().join("session"),
            "a",
            &["notice_fired"],
            Act::answer(json!({"text": "a"})),
        );
        layer(
            &t.path().join("session"),
            "b",
            &["notice_fired"],
            Act::mask(),
        );
        let mut s = stack(&t);
        assert_eq!(s.call("notice_fired", &json!({})).answer, None, "b masks");

        s.set_disabled("s2", true).unwrap();
        let r = s.call("notice_fired", &json!({}));
        assert_eq!(answer_of(&r), json!({"text": "a"}));
        assert_eq!(r.by.as_deref(), Some("s1"));
        assert_eq!(
            s.layers().len(),
            2,
            "disabled is not deleted: the stack still reads as history"
        );
        assert!(s.layer("s2").unwrap().disabled);

        s.set_disabled("s2", false).unwrap();
        assert_eq!(s.call("notice_fired", &json!({})).answer, None);
        assert!(
            s.set_disabled("s9", true).is_err(),
            "an id nobody has is refused"
        );
    }

    #[test]
    fn layer_ids_are_creation_order_within_a_scope_and_newest_is_on_top() {
        let t = TempDir::new("order");
        let session = t.path().join("session");
        // Written in name order, so the mtime order and the name tie-break agree and the
        // assertion holds on a filesystem whose timestamps are coarse.
        for name in ["a", "b", "c"] {
            layer(
                &session,
                name,
                &["notice_fired"],
                Act::answer(json!({"text": name})),
            );
        }
        let s = stack(&t);
        assert_eq!(
            s.layers().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["s3", "s2", "s1"],
        );
        assert_eq!(
            answer_of(&s.call("notice_fired", &json!({}))),
            json!({"text": "c"})
        );
    }

    #[test]
    fn a_directory_without_a_manifest_is_a_failure_and_a_directory_named_in_it_is_reported() {
        let t = TempDir::new("no-manifest");
        let session = t.path().join("session");
        let broken = session.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("plugin.toml"), "name = ").unwrap();
        let s = stack(&t);
        assert!(s.layers().is_empty(), "no manifest is not a layer");
        assert_eq!(s.failures().len(), 1);
        assert_eq!(s.failures()[0].layer, "broken");
        assert_eq!(s.failures()[0].scope, Scope::Session);
        assert!(
            s.failures()[0].why.contains("plugin.toml"),
            "{}",
            s.failures()[0]
        );
    }

    #[test]
    fn an_engine_that_refuses_a_module_is_a_recorded_failure_not_a_missing_stack() {
        let t = TempDir::new("engine-refuses");
        let session = t.path().join("session");
        layer(
            &session,
            "a",
            &["notice_fired"],
            Act::answer(json!({"text": "a"})),
        );
        let loader = std::sync::Arc::new(crate::testing::FakeLoader::failing(
            "import `wasi_snapshot_preview1` is not granted by [read]",
        ));
        let s = Stack::discover(Some(&session), None, None, loader);
        assert_eq!(s.layers().len(), 1, "the layer exists; its module does not");
        assert_eq!(s.failures().len(), 1);
        assert!(
            s.failures()[0].why.contains("wasi_snapshot_preview1"),
            "{}",
            s.failures()[0]
        );
        assert_eq!(s.call("notice_fired", &json!({})).answer, None);
    }

    #[test]
    fn discovery_hands_the_manifest_capabilities_to_the_engine() {
        let t = TempDir::new("caps");
        let session = t.path().join("session");
        crate::testing::layer_with(
            &session,
            "a",
            &["condition_github_pr"],
            &["read", "exec:gh"],
            Act::answer(json!({"fired": null})),
        );
        let loader = std::sync::Arc::new(crate::testing::FakeLoader::new());
        let s = Stack::discover(Some(&session), None, None, loader.clone());
        assert_eq!(s.layers().len(), 1);
        let caps = loader
            .caps_of(&session.join("a").join("plugin.wasm"))
            .expect("the engine was asked to load it");
        assert_eq!(
            caps,
            vec![Capability::Read, Capability::Exec("gh".to_string())],
            "a module is handed exactly what its manifest declared and nothing more"
        );
    }

    #[test]
    fn a_layer_whose_module_will_not_load_stays_listed_and_never_answers() {
        let t = TempDir::new("bad-wasm");
        let session = t.path().join("session");
        let dir = session.join("a");
        layer(
            &session,
            "a",
            &["notice_fired"],
            Act::answer(json!({"text": "a"})),
        );
        std::fs::write(dir.join("plugin.wasm"), "this is not a fake module").unwrap();
        let s = stack(&t);
        assert_eq!(s.layers().len(), 1);
        assert_eq!(s.layers()[0].id, "s1");
        assert_eq!(
            s.failures().len(),
            1,
            "the reason is on the stack, not in every call"
        );
        assert_eq!(s.call("notice_fired", &json!({})).answer, None);
    }

    #[test]
    fn poll_changes_sees_a_rewritten_plugin_wasm_and_nothing_else() {
        let t = TempDir::new("poll");
        let session = t.path().join("session");
        layer(
            &session,
            "a",
            &["notice_fired"],
            Act::answer(json!({"text": "a"})),
        );
        layer(
            &session,
            "b",
            &["notice_fired"],
            Act::answer(json!({"text": "b"})),
        );
        let mut s = stack(&t);
        assert!(
            s.poll_changes().is_empty(),
            "a quiet stack has no candidates"
        );

        // Longer than the first script, so the poll's length check alone would catch the rewrite
        // on a filesystem whose mtimes are coarse.
        write_wasm(
            &session.join("a"),
            &["notice_fired"],
            Act::answer(json!({"text": "a, rebuilt and rather longer than it was"})),
        );
        let pending = s.poll_changes();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].layer, "s1");
        assert_eq!(pending[0].scope, Scope::Session);
        assert_ne!(pending[0].sha256, s.layer("s1").unwrap().sha256);

        // Still pending: a change that has not been applied has not happened.
        assert_eq!(s.poll_changes().len(), 1);

        s.apply(pending.into_iter().next().unwrap()).unwrap();
        // `b` is on top and answers first; turning it off is how the rebuilt `a` is heard.
        s.set_disabled("s2", true).unwrap();
        assert_eq!(
            answer_of(&s.call("notice_fired", &json!({}))),
            json!({"text": "a, rebuilt and rather longer than it was"})
        );
        assert!(s.poll_changes().is_empty(), "applied is settled");
    }

    #[test]
    fn apply_refuses_a_module_that_moved_after_the_gate_replayed_it() {
        let t = TempDir::new("stale");
        let session = t.path().join("session");
        layer(
            &session,
            "a",
            &["notice_fired"],
            Act::answer(json!({"text": "a"})),
        );
        let mut s = stack(&t);
        write_wasm(
            &session.join("a"),
            &["notice_fired"],
            Act::answer(json!({"text": "rebuilt"})),
        );
        let pending = s.poll_changes().into_iter().next().unwrap();
        // What the gate does: it loads the bytes and remembers them on the candidate.
        let module: Box<dyn Module> = Box::new(Fake::one(
            "notice_fired",
            Act::answer(json!({"text": "rebuilt"})),
        ));
        pending.remember("a-different-sha", Arc::from(module));
        let why = s
            .apply(pending)
            .expect_err("not the bytes that were replayed");
        assert!(why.contains("gate it again"), "{why}");
    }

    #[test]
    fn a_new_directory_becomes_a_layer_on_top_of_its_scope() {
        let t = TempDir::new("new-layer");
        let session = t.path().join("session");
        layer(
            &session,
            "a",
            &["notice_fired"],
            Act::answer(json!({"text": "a"})),
        );
        let mut s = stack(&t);
        let fresh = session.join("b");
        layer(
            &session,
            "b",
            &["notice_fired"],
            Act::answer(json!({"text": "b"})),
        );
        let c = s.candidate(&fresh, Scope::Session).unwrap();
        assert_eq!(
            c.layer, "",
            "a directory the stack has never seen has no id yet"
        );
        s.apply(c).unwrap();
        assert_eq!(s.layers()[0].id, "s2");
        assert_eq!(
            answer_of(&s.call("notice_fired", &json!({}))),
            json!({"text": "b"})
        );
    }
}
