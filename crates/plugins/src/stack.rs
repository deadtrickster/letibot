//! The stack: what layers exist, which one answers a hook, and what a change costs.
//! Scopes in precedence: session, project, user, built-in; newest first within a scope.
//! A directory without a readable `plugin.toml` is a `Failure`; a manifest with no module is a declaration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::Value;

use crate::Capability;
use crate::manifest::{Manifest, Scope, ToolDecl};
use crate::trust::{Pin, Trust};
use crate::{Loader, MANIFEST, Module, sha256_file};

/// How many module calls one resolution may make. An ask-below nests, so a stack of ten layers
/// could in principle ask for a great many answers to settle one hook; the budget turns that into
/// a recorded failure with an answer from below, instead of a pane that takes a second to draw.
const MAX_CALLS: usize = 32;

/// One tool a layer declares, with the layer it came from and what that layer may read.
/// A [`ToolDecl`] alone is prompt bytes; a seated tool needs the layer (for a failure's sentence),
/// what it may read (a hook's looks are checked against the layer's capabilities) and its scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredTool {
    /// The layer's directory name for a layer on disk, the built-in's name for one compiled in.
    pub layer: String,
    pub scope: Scope,
    pub tool: ToolDecl,
    pub capabilities: Vec<Capability>,
}

/// A layer: where it came from, what it declares, and which bytes it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// `s3`, `p1`, `u1` - or `b-github-pr` for a built-in.
    pub id: String,
    pub scope: Scope,
    /// The layer's directory. Empty for a built-in, which has none.
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// The SHA-256 of the layer's module file, named by [`Manifest::module`]; empty when there is
    /// no module to hash.
    pub sha256: String,
    /// The SHA-256 of `plugin.toml`. A pin is a statement about BOTH files, so this is what
    /// [`Layer::trusted`] is decided against beside `sha256`.
    pub manifest_sha256: String,
    pub disabled: bool,
    /// Whether this layer may run freely: true for a built-in, and for a layer whose module AND
    /// manifest match a recorded pin. Where a file sits proves nothing, so the fact is the hash.
    /// An untrusted layer still answers, but every call it makes is adjudicated without a card.
    pub trusted: bool,
    /// The pin nearest to this layer: the one its bytes match, or - with `trusted` false - the one
    /// they have broken. `None` for a layer nothing was ever pinned for, and for a built-in.
    pub pin: Option<Pin>,
}

impl Layer {
    /// What a refusal of this layer says - the layer by name, and both hashes when there is a
    /// pin to compare against ([`crate::Standing::sentence`]).
    pub fn untrusted_sentence(&self) -> String {
        crate::Standing {
            free: self.trusted,
            pin: self.pin.clone(),
        }
        .sentence(&self.manifest.name, &self.sha256, &self.manifest_sha256)
    }
}

/// A pin whose bytes moved: the layer, what was pinned, and what is on disk now.
/// Drained by the host, which says it once (`plugin_untrusted`): a layer that quietly stopped
/// being yours is the state the ladder exists to make impossible to miss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenPin {
    pub layer: String,
    pub scope: Scope,
    pub name: String,
    pub pin: Pin,
    pub module: String,
    pub manifest: String,
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
    /// The layer whose answer it is - the topmost one, since a layer that asks below still
    /// answers for itself.
    pub by: Option<String>,
    pub failures: Vec<Failure>,
    /// Set for a guard hook (`on_tool_call`) when a layer failed or the resolution gave up:
    /// the layer's id and why. The caller refuses the call; a failed guard never reads as a pass.
    pub refused: Option<(String, String)>,
}

/// Hooks whose failure must stop what they guard rather than let it through.
const FAIL_CLOSED: &[&str] = &["on_tool_call"];

/// A layer's module changed on disk and is waiting for the gate.
/// The gate fills in the module it loaded and `Stack::apply` swaps in exactly that one: a second
/// compile of the same bytes would mean the module that answers was not the one replayed.
pub struct Candidate {
    /// The id of the layer this replaces, or empty for a layer that does not exist yet.
    pub layer: String,
    pub scope: Scope,
    pub dir: PathBuf,
    /// The manifest read when the change was noticed.
    pub manifest: Manifest,
    /// The `plugin.wasm` the poll saw - before the gate's build rewrote it.
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

    /// Whether a gate has run over this candidate at all - which is what tells "the file moved
    /// after the gate" apart from "nothing was gated here".
    fn was_replayed(&self) -> bool {
        matches!(self.replayed.lock(), Ok(slot) if slot.is_some())
    }

    /// Whether a gate has run over this candidate at all - the public half of the check
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

/// What a layer's module looked like the last time the stack read it. The SHA-256 is the truth;
/// the mtime and the length are the cheap half of a poll - a file that moved in neither cannot
/// have different bytes, so a poll over a quiet stack stats files and hashes none of them.
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

/// Whether the module a layer's manifest names is on disk at all. `false` for a path that is not
/// there, and for nothing else: a module that exists and will not load is a `Failure`. A dangling
/// symlink counts as there, so the reason is recorded once rather than as a layer with no module.
fn has_module(path: &Path) -> bool {
    !matches!(
        std::fs::symlink_metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// Everything about a layer that is not in the [`Layer`] itself: its module, the hooks that
/// module exports, and the bytes it was read from.
#[derive(Clone)]
struct Wired {
    /// `None` when the module could not be loaded or admitted - the reason is then in the stack's
    /// failures - and for a layer that has no module at all (a declaration, which has no reason to
    /// give). Either way the layer still appears in the stack view.
    module: Option<Arc<dyn Module>>,
    hooks: Vec<String>,
    seen: Fingerprint,
}

/// The four scopes resolved into one stack.
/// Layered as the module doc says, and cloneable so the gate can fork one: the fork is the shadow,
/// and the live stack it was cloned from is untouched.
#[derive(Clone)]
pub struct Stack {
    /// Newest-first within scope, scopes in precedence order. This vector *is* the order a hook
    /// is resolved in. The modules live beside it, keyed by id, so cloning a stack shares them
    /// (an `Arc` each) and no two orderings can drift apart.
    layers: Vec<Layer>,
    wired: BTreeMap<String, Wired>,
    loader: Arc<dyn Loader>,
    failures: Vec<Failure>,
    /// The pin book - which layers' bytes a person has pinned. `None` on a stack nobody gave
    /// one to, and then every placed layer is unpinned, which is the fail-closed direction.
    trust: Option<Arc<Trust>>,
    /// Pins already reported as broken, so a poll every two seconds does not say it every time.
    said_pins: std::collections::BTreeSet<String>,
    /// File listing the disabled built-in ids, one per line. `None` keeps built-in state in memory.
    builtin_state: Option<PathBuf>,
}

/// A disk layer's off switch: this file in its directory. It lives and dies with the layer.
pub const DISABLED_MARK: &str = ".disabled";

impl Stack {
    /// Discover layers from the four scopes. Each argument is the root of a scope and may be
    /// absent, which is an empty scope and not an error: session under `session_dir` itself,
    /// project under `<project>/.letibot/plugins`, user under `<user>/plugins`. Built-ins come after.
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
            trust: None,
            said_pins: std::collections::BTreeSet::new(),
            builtin_state: None,
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

    /// Keep the disabled built-ins in `path`, and apply it to built-ins registered from now on.
    pub fn keep_builtin_state(&mut self, path: PathBuf) {
        self.builtin_state = Some(path);
    }

    /// Hand this stack the trust book, and decide every placed layer against it. A stack nobody
    /// gives a book to has every placed layer untrusted - the fail-closed direction, and why this is
    /// a setter. The answer is news the caller must say: a dropped `Vec` silences the restart warning.
    pub fn set_trust(&mut self, trust: Arc<Trust>) -> Vec<BrokenPin> {
        self.trust = Some(trust);
        self.refresh_trust()
    }

    /// The book this stack decides against, if it has one.
    pub fn trust(&self) -> Option<&Arc<Trust>> {
        self.trust.as_ref()
    }

    /// Re-decide every placed layer's `trusted` against the book, and answer the pins whose bytes
    /// have moved. Cheap enough for a two-second poll: a hash of each `plugin.toml` and the module's
    /// hash the stack already holds. The answer is what is NEW, said once by the caller.
    pub fn refresh_trust(&mut self) -> Vec<BrokenPin> {
        let Some(trust) = self.trust.clone() else {
            return Vec::new();
        };
        trust.refresh();
        let mut news = Vec::new();
        for layer in &mut self.layers {
            if layer.scope == Scope::BuiltIn || layer.dir.as_os_str().is_empty() {
                continue;
            }
            let manifest_sha256 =
                sha256_file(&layer.dir.join(MANIFEST)).unwrap_or_else(|_| String::new());
            let standing = trust.standing(&layer.dir, &layer.sha256, &manifest_sha256);
            let was = layer.trusted;
            layer.trusted = standing.free;
            layer.pin = standing.pin.clone();
            layer.manifest_sha256 = manifest_sha256.clone();
            // A pin that was broken is news; a layer nobody pinned is the ordinary state.
            if let (false, Some(pin)) = (standing.free, standing.pin) {
                let key = format!("{}\n{}\n{manifest_sha256}", layer.id, layer.sha256);
                if (was || !self.said_pins.contains(&key)) && self.said_pins.insert(key) {
                    news.push(BrokenPin {
                        layer: layer.id.clone(),
                        scope: layer.scope,
                        name: layer.manifest.name.clone(),
                        pin,
                        module: layer.sha256.clone(),
                        manifest: manifest_sha256,
                    });
                }
            }
        }
        news
    }

    /// The layer a person or a tool named - by id (`p1`), by its manifest name, by the directory it
    /// sits in, or by that directory's own name. `None` when nothing matches, never a guess between
    /// two: a caller that has to choose should have named an id.
    pub fn layer_named(&self, what: &str) -> Option<&Layer> {
        let dir = Path::new(what);
        self.layers
            .iter()
            .find(|l| l.id == what)
            .or_else(|| self.layers.iter().find(|l| l.manifest.name == what))
            .or_else(|| {
                self.layers
                    .iter()
                    .find(|l| !l.dir.as_os_str().is_empty() && l.dir == dir)
            })
            // The name `declared_tools` carries is the layer's DIRECTORY name, not its path -
            // a tool's `DeclaredTool::layer` is what a refusal names, so this spelling has to
            // find the layer it came from.
            .or_else(|| {
                self.layers.iter().find(|l| {
                    l.dir.file_name().is_some_and(|n| n == what) && !l.dir.as_os_str().is_empty()
                })
            })
    }

    /// Every layer a person could pin, in stack order - what `/plugin` lists when it is asked
    /// with no name: the id, the name, whether it is free, and the two hashes.
    pub fn placed(&self) -> Vec<&Layer> {
        self.layers
            .iter()
            .filter(|l| l.scope != Scope::BuiltIn)
            .collect()
    }

    /// What the layers of these three scopes declare, without loading a single module: a declared
    /// tool is prompt bytes and the prompt is composed before any host exists. No dedupe and no
    /// built-in scope - the order IS the precedence, so the caller keeps the first of each name.
    pub fn declared_tools(
        session_dir: Option<&Path>,
        project: Option<&Path>,
        user: Option<&Path>,
    ) -> Vec<DeclaredTool> {
        let mut out = Vec::new();
        let scopes = [
            (Scope::Session, session_dir.map(Path::to_path_buf)),
            (
                Scope::Project,
                project.map(|root| root.join(".letibot").join("plugins")),
            ),
            (Scope::User, user.map(|root| root.join("plugins"))),
        ];
        for (scope, dir) in scopes {
            let Some(dir) = dir else { continue };
            // Newest first: a later layer is on top, so its declaration is the one the model is
            // offered when two of them name the same tool.
            for path in layer_dirs(&dir).into_iter().rev() {
                let Ok(manifest) = Manifest::read(&path) else {
                    continue;
                };
                if manifest.tools.is_empty() {
                    continue;
                }
                let layer = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                for tool in manifest.tools.clone() {
                    out.push(DeclaredTool {
                        layer: layer.clone(),
                        scope,
                        tool,
                        capabilities: manifest.capabilities.clone(),
                    });
                }
            }
        }
        out
    }

    /// Every layer, in the order a hook is resolved through them.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// The engine this stack loads its modules with - the gate's step 2 and `apply`'s ungated
    /// path both go through the stack's own loader, so a shadow is compiled by the same engine as
    /// the live stack.
    pub(crate) fn loader(&self) -> &Arc<dyn Loader> {
        &self.loader
    }

    /// What discovery, `apply` and `poll_changes` could not load. A failure met while resolving a
    /// hook is in [`Resolved::failures`] instead - see the module doc.
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }

    /// One layer by id.
    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// The fuel a layer's module burned on its last call - the price per hook, which `plugin test`
    /// reports. `None` for a module that never loaded: *nothing was measured* is not *it cost
    /// nothing*. The number is the engine's last call, so ask it right after the call you care about.
    pub fn fuel(&self, id: &str) -> Option<u64> {
        self.wired
            .get(id)
            .and_then(|w| w.module.as_ref())
            .map(|m| m.fuel())
    }

    /// Register a built-in default: a layer the host compiled in, with the module that answers for
    /// it. The last registered is on top of the built-in scope, so register the least specific
    /// first. An empty id becomes `b-<name>`, and a taken id is refused: two layers cannot be one.
    pub fn register_builtin(&mut self, layer: Layer, module: Box<dyn Module>) {
        self.register_builtin_shared(layer, Arc::from(module));
    }

    /// `register_builtin` with a module the caller already holds behind an `Arc`. One `Arc`,
    /// several stacks: a `Module` answers `&self`, so a session's stack, the gate's shadow and the
    /// next session may share the same compiled bytes, and compiling one is the expensive half.
    pub fn register_builtin_shared(&mut self, mut layer: Layer, module: Arc<dyn Module>) {
        // A built-in runs freely, by definition. It is the binary's own
        // bytes: nobody placed them at run time, so there is nothing to pin.
        layer.trusted = true;
        layer.pin = None;
        if layer.id.is_empty() {
            layer.id = format!("{}-{}", Scope::BuiltIn.prefix(), layer.manifest.name);
        }
        layer.scope = Scope::BuiltIn;
        if self.disabled_builtins().contains(&layer.id) {
            layer.disabled = true;
        }
        if self.layers.iter().any(|l| l.id == layer.id) {
            self.failures.push(Failure {
                layer: layer.id.clone(),
                scope: Scope::BuiltIn,
                hook: None,
                why: "a built-in with this id is already registered".to_string(),
            });
            return;
        }
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

    /// Register a built-in the binary carries as BYTES, read into the bottom of the stack. The
    /// manifest is parsed from its text and the module comes from memory, so it goes through the
    /// same doors a layer on disk does. A failure is answered AND recorded, as discovery does.
    pub fn register_builtin_bytes(
        &mut self,
        name: &str,
        manifest: &str,
        wasm: &[u8],
    ) -> Result<(), String> {
        let parsed = Manifest::parse(manifest).map_err(|why| format!("plugin.toml: {why}"))?;
        let module = self
            .loader
            .load_builtin(name, wasm, &parsed.capabilities)
            .map_err(|why| {
                let why = format!("the module could not be loaded: {why}");
                self.failures.push(Failure {
                    layer: format!("{}-{name}", Scope::BuiltIn.prefix()),
                    scope: Scope::BuiltIn,
                    hook: None,
                    why: why.clone(),
                });
                why
            })?;
        let layer = Layer {
            // Filled in here rather than by `register_builtin`, so the failure this call may
            // record can be found by the id the caller will see in the stack view.
            id: format!("{}-{}", Scope::BuiltIn.prefix(), parsed.name),
            scope: Scope::BuiltIn,
            dir: PathBuf::new(),
            manifest: parsed,
            sha256: crate::sha256_hex(wasm),
            manifest_sha256: crate::sha256_hex(manifest.as_bytes()),
            disabled: false,
            trusted: true,
            pin: None,
        };
        let id = layer.id.clone();
        let before = self.failures.len();
        self.register_builtin_shared(layer, module);
        // `register_builtin` records its own refusals; answer the one that belongs to THIS call
        // so a caller is not told *ok* about a layer that is not on the stack.
        match self.failures[before..].iter().find(|f| f.layer == id) {
            Some(f) => Err(f.why.clone()),
            None => Ok(()),
        }
    }

    /// Turn a layer off (or on) without deleting it - `plugin disable s2`, the bisect for
    /// *"which of my fixes broke this"*. A disabled layer stays in the stack view, greyed, which
    /// is the point: the stack reads as history, and history that vanished cannot be read.
    pub fn set_disabled(&mut self, id: &str, disabled: bool) -> Result<(), String> {
        let i = self.index_of(id)?;
        self.persist_disabled(i, disabled)?;
        self.layers[i].disabled = disabled;
        Ok(())
    }

    /// [`Stack::set_disabled`] for this stack value only, nothing written. For a fork that masks
    /// layers to ask one question.
    pub fn mask(&mut self, id: &str, disabled: bool) -> Result<(), String> {
        let i = self.index_of(id)?;
        self.layers[i].disabled = disabled;
        Ok(())
    }

    fn index_of(&self, id: &str) -> Result<usize, String> {
        self.layers.iter().position(|l| l.id == id).ok_or_else(|| {
            let ids: Vec<&str> = self.layers.iter().map(|l| l.id.as_str()).collect();
            format!("no layer `{id}` — the stack has [{}]", ids.join(", "))
        })
    }

    /// A disk layer carries its mark in its own directory, so a deleted layer drops it. A built-in
    /// has no directory, so its state goes in the stack's state file when it has one.
    fn persist_disabled(&self, i: usize, disabled: bool) -> Result<(), String> {
        let layer = &self.layers[i];
        if layer.scope == Scope::BuiltIn {
            let Some(path) = self.builtin_state.as_ref() else {
                return Ok(());
            };
            let mut ids = self.disabled_builtins();
            if disabled {
                ids.insert(layer.id.clone());
            } else {
                ids.remove(&layer.id);
            }
            let text: String = ids.iter().map(|id| format!("{id}\n")).collect();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            return std::fs::write(path, text)
                .map_err(|e| format!("could not save {}: {e}", path.display()));
        }
        if layer.dir.as_os_str().is_empty() {
            return Ok(());
        }
        let mark = layer.dir.join(DISABLED_MARK);
        let done = if disabled {
            std::fs::write(&mark, "")
        } else {
            match std::fs::remove_file(&mark) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        };
        done.map_err(|e| format!("could not save {}: {e}", mark.display()))
    }

    fn disabled_builtins(&self) -> std::collections::BTreeSet<String> {
        self.builtin_state
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| {
                t.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolve one hook through the stack, from the top: pass or unchanged falls through, mask stops
    /// everything below, `below` round trips, anything else answers. A failed layer is recorded and
    /// skipped, except a guard hook ([`FAIL_CLOSED`]), where a failure sets `refused`.
    pub fn call(&self, hook: &str, input: &Value) -> Resolved {
        let mut walk = Walk {
            hook,
            failures: Vec::new(),
            calls: 0,
            spent: false,
            closed: FAIL_CLOSED.contains(&hook),
            broke: None,
        };
        let (answer, by, refused) = match self.run(0, input, &mut walk) {
            Step::Answer { value, by } => (Some(value), Some(by), None),
            Step::Failed { by, why } => (None, None, Some((by, why))),
            Step::Passed | Step::Masked => (None, None, None),
        };
        Resolved {
            answer,
            by,
            failures: walk.failures,
            refused,
        }
    }

    /// Re-stat each layer's module; the ones whose bytes changed come back as candidates. The file
    /// it stats is the one the manifest names, so a layer answering from a script is polled on the
    /// script. A change is pending until applied, so it is reported again; no module at all is silent.
    pub fn poll_changes(&mut self) -> Vec<Candidate> {
        let mut out = Vec::new();
        for i in 0..self.layers.len() {
            let layer = &self.layers[i];
            if layer.scope == Scope::BuiltIn || layer.dir.as_os_str().is_empty() {
                continue;
            }
            let wasm = layer.manifest.module_path(&layer.dir);
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
        let sha256 = sha256_file(&manifest.module_path(dir))
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

    /// Put a candidate into the stack - only after the gate passed. Two things are refused here
    /// that the gate cannot see: a `plugin.wasm` that is no longer the bytes the gate replayed,
    /// because what answers must be what was replayed, and a manifest that no longer admits it.
    pub fn apply(&mut self, c: Candidate) -> Result<(), String> {
        if c.scope == Scope::BuiltIn {
            return Err(
                "a built-in has no directory to reload — register_builtin is that door".to_string(),
            );
        }
        let dir = c.dir.clone();
        let manifest = Manifest::read(&dir)?;
        let wasm = manifest.module_path(&dir);
        // A module this build cannot run never applies, in the words the loader would use.
        crate::engine_for(&manifest.module)?;
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
                // A rebuild is a different layer to the pin book: the pin was about the old
                // bytes, so this one is decided again - and if it was pinned, the fact that the
                // pin no longer holds is news the next `refresh_trust` says.
                let manifest_sha256 =
                    sha256_file(&dir.join(MANIFEST)).unwrap_or_else(|_| String::new());
                let (trusted, pin) =
                    self.standing_of(&dir, &self.layers[i].sha256, &manifest_sha256);
                self.layers[i].manifest_sha256 = manifest_sha256;
                self.layers[i].trusted = trusted;
                self.layers[i].pin = pin;
                self.wired.insert(id, wired);
            }
            None => {
                let id = self.next_id(c.scope);
                let manifest_sha256 =
                    sha256_file(&dir.join(MANIFEST)).unwrap_or_else(|_| String::new());
                let (trusted, pin) = self.standing_of(&dir, &seen.sha256, &manifest_sha256);
                let disabled = dir.join(DISABLED_MARK).is_file();
                let layer = Layer {
                    id: id.clone(),
                    scope: c.scope,
                    dir,
                    manifest,
                    sha256: seen.sha256,
                    manifest_sha256,
                    disabled,
                    // See `Layer::trusted`: a new directory is the operator's only once the book
                    // says these bytes are theirs.
                    trusted,
                    pin,
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

    /// The second arm of the trust ladder, in one place: what the book says about a directory's
    /// bytes. `(false, None)` for a stack with no book, so a caller that never handed one over gets
    /// every placed layer adjudicated - the direction a mistake has to fail in.
    fn standing_of(&self, dir: &Path, module: &str, manifest: &str) -> (bool, Option<Pin>) {
        match self.trust.as_ref() {
            Some(trust) => {
                let standing = trust.standing(dir, module, manifest);
                (standing.free, standing.pin)
            }
            None => (false, None),
        }
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
            let wasm = manifest.module_path(&path);
            // A manifest and no module is a declaration, not a failure: recording it would publish
            // `plugin_layer` warnings for a CORRECT install of a built-in's tool, and there is
            // nothing to load. `poll_changes` picks a module up if one is ever written beside it.
            let (seen, module, hooks) = if !has_module(&wasm) {
                (Fingerprint::absent(), None, Vec::new())
            } else {
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
                let module = match self.load_placed(&manifest, &wasm) {
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
                match module {
                    Some((module, hooks)) => (seen, Some(module), hooks),
                    None => (seen, None, Vec::new()),
                }
            };
            let manifest_sha256 =
                sha256_file(&path.join(MANIFEST)).unwrap_or_else(|_| String::new());
            let (trusted, pin) = self.standing_of(&path, &seen.sha256, &manifest_sha256);
            let disabled = path.join(DISABLED_MARK).is_file();
            self.layers.push(Layer {
                id: id.clone(),
                scope,
                dir: path,
                sha256: seen.sha256.clone(),
                manifest,
                // A placed layer is not the operator's because of where it sits: the hashes in the
                // book decide, and `set_trust` (or the poll) fills these in - a stack nobody gave a
                // book to leaves every layer untrusted, which is the fail-closed direction.
                manifest_sha256,
                disabled,
                trusted,
                pin,
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

    /// One placed layer's module, or the reason it has none. The engine question comes first: a
    /// manifest may name a module that is not wasm, and this build has one engine, so a `.js` layer
    /// is on the ladder (pinned, hashed, polled) and answers nothing rather than half-loading.
    fn load_placed(&self, manifest: &Manifest, wasm: &Path) -> Result<Box<dyn Module>, String> {
        crate::engine_for(&manifest.module)?;
        self.loader.load(wasm, &manifest.capabilities)
    }

    fn run(&self, from: usize, input: &Value, walk: &mut Walk<'_>) -> Step {
        for i in from..self.layers.len() {
            if walk.spent {
                return walk.step_after_failure();
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

    /// One layer's answer, including the one ask-below it may take.
    fn ask(
        &self,
        i: usize,
        module: &Arc<dyn Module>,
        input: &Value,
        asked: bool,
        walk: &mut Walk<'_>,
    ) -> Step {
        let layer = &self.layers[i];
        if walk.spent {
            return walk.step_after_failure();
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
            return walk.step_after_failure();
        }
        walk.calls += 1;
        let text = match module.call(walk.hook, &input.to_string()) {
            Ok(Some(text)) => text,
            Ok(None) => return Step::Passed,
            Err(why) => {
                walk.fail(layer, why);
                return walk.step_after_failure();
            }
        };
        let Some(answer) = parse(&text, layer, walk) else {
            return walk.step_after_failure();
        };
        if is(&answer, "mask") {
            return Step::Masked;
        }
        if let Some(below_input) = answer.get("below") {
            if asked {
                walk.fail(
                    layer,
                    "a second `below` inside one resolution — a layer may ask below once"
                        .to_string(),
                );
                return walk.step_after_failure();
            }
            // The layer below is resolved and its answer is handed back to this layer, which is
            // asked again with the same input plus `below`. A mask or a pass below is `null`:
            // the lower stack answered nothing, and the layer that asked is told so.
            let mut below_failed: Option<Step> = None;
            let below = match self.run(i + 1, below_input, walk) {
                Step::Answer { value, .. } => value,
                f @ Step::Failed { .. } => {
                    below_failed = Some(f);
                    Value::Null
                }
                Step::Passed | Step::Masked => Value::Null,
            };
            let Some(mut second) = input.as_object().cloned() else {
                walk.fail(
                    layer,
                    "asked below, but the hook's input is not an object, so `below` has nowhere to go"
                        .to_string(),
                );
                return walk.step_after_failure();
            };
            second.insert("below".to_string(), below);
            let second = Value::Object(second);
            let step = self.ask(i, module, &second, true, walk);
            // A failed guard below still closes the call, unless the layer that asked answered
            // with a refusal or its own answer: neither lets the call run.
            return match (step, below_failed) {
                (step @ Step::Answer { .. }, Some(f)) => {
                    let stops = matches!(&step, Step::Answer { value, .. }
                        if value.get("refuse").is_some_and(|v| !v.is_null())
                            || value.get("answer").is_some_and(|v| !v.is_null()));
                    if stops { step } else { f }
                }
                (Step::Passed, Some(f)) => f,
                (step, _) => step,
            };
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
    Answer {
        value: Value,
        by: String,
    },
    Passed,
    Masked,
    /// A guard hook's layer failed: the caller refuses what the hook guards.
    Failed {
        by: String,
        why: String,
    },
}

/// The mutable half of one resolution: what it has recorded, and whether it still may ask.
struct Walk<'a> {
    hook: &'a str,
    failures: Vec<Failure>,
    calls: usize,
    spent: bool,
    /// The hook is a guard: a failure refuses instead of passing.
    closed: bool,
    /// The layer and reason of the last failure.
    broke: Option<(String, String)>,
}

impl Walk<'_> {
    /// What a failed layer yields: a pass for an advisory hook, a refusal for a guard.
    fn step_after_failure(&self) -> Step {
        match (&self.broke, self.closed) {
            (Some((by, why)), true) => Step::Failed {
                by: by.clone(),
                why: why.clone(),
            },
            _ => Step::Passed,
        }
    }

    fn fail(&mut self, layer: &Layer, why: String) {
        self.broke = Some((layer.id.clone(), why.clone()));
        self.failures.push(Failure {
            layer: layer.id.clone(),
            scope: layer.scope,
            hook: Some(self.hook.to_string()),
            why,
        });
    }
}

/// A layer's answer as JSON, or nothing plus a recorded failure. Malformed output is the one the
/// plan names explicitly: it is a bug in the plugin, and the layer below still
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
    use crate::testing::{Act, Fake, TempDir, layer, script, write_wasm, write_wasm_bytes};
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

    /// A layer whose manifest NAMES its module: a script, which this build cannot run.
    fn script_layer(root: &Path, name: &str, module: &str, body: &[u8]) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("a layer directory");
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "name = \"{name}\"\npurpose = \"p\"\nhooks = [\"notice_fired\"]\ncapabilities = []\ncontract = 1\nmodule = \"{module}\"\n"
            ),
        )
        .expect("plugin.toml");
        std::fs::write(dir.join(module), body).expect("the named module");
        dir
    }

    // -----------------------------------------------------------------------------------------
    // `module`: the file a layer answers from, which is not always plugin.wasm
    // -----------------------------------------------------------------------------------------

    /// A script layer is on the ladder and answers nothing. It is pinned, hashed and polled
    /// like any other (`docs/plugins-plan.md` step 11's decision 2); what it cannot do is run,
    /// because no engine for it is built in. Saying so at load is what keeps fail-down true.
    #[test]
    fn a_script_module_falls_down_and_says_no_engine_for_it_is_built_in() {
        let t = TempDir::new("script-module");
        let dir = script_layer(
            &t.path().join("session"),
            "fix",
            "fix.js",
            b"export function notice_fired() { return null }\n",
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "below",
            &["notice_fired"],
            Act::answer(json!({"from": "below"})),
        );
        let s = stack(&t);

        // A layer, hashed, and the hash is the script's rather than a plugin.wasm that is not there.
        let placed = s.layer("s1").expect("the script layer is a layer");
        assert_eq!(placed.dir, dir);
        assert_eq!(
            placed.sha256,
            crate::sha256_hex(b"export function notice_fired() { return null }\n")
        );
        // And it says why it cannot answer, once, on the stack.
        assert_eq!(s.failures().len(), 1, "{:?}", s.failures());
        let why = &s.failures()[0].why;
        assert!(why.contains("fix.js"), "{why}");
        assert!(
            why.contains("no engine for it is built into this host"),
            "{why}"
        );
        // The layer below answers, which is fail-down and not a stopped resolution.
        let r = s.call("notice_fired", &json!({}));
        assert_eq!(r.answer, Some(json!({ "from": "below" })));
        assert_eq!(r.by.as_deref(), Some("p1"));
    }

    #[test]
    fn poll_changes_watches_the_module_the_manifest_names() {
        let t = TempDir::new("poll-named");
        let dir = script_layer(&t.path().join("session"), "fix", "fix.js", b"v1\n");
        // A `plugin.wasm` beside it is not this layer's module, so it is not what the poll reads.
        write_wasm_bytes(
            &dir,
            &script(&[("notice_fired", Act::answer(json!({"v": 1})))]),
        );
        let mut s = stack(&t);
        assert!(
            s.poll_changes().is_empty(),
            "a quiet stack has no candidates"
        );

        std::fs::write(dir.join("plugin.wasm"), b"a build of something else").expect("a wasm");
        assert!(
            s.poll_changes().is_empty(),
            "the module the manifest names is the one watched"
        );

        std::fs::write(dir.join("fix.js"), b"v2, a second version\n").expect("an edit");
        let pending = s.poll_changes();
        assert_eq!(pending.len(), 1, "the named module moved");
        assert_eq!(pending[0].layer, "s1");
        assert_eq!(
            pending[0].sha256,
            crate::sha256_hex(b"v2, a second version\n")
        );
        // And a module this build cannot run never applies, in the words the loader would use.
        let why = s
            .apply(pending.into_iter().next().unwrap())
            .expect_err("a script cannot be applied");
        assert!(
            why.contains("no engine for it is built into this host"),
            "{why}"
        );
    }

    /// A script layer is pinnable, and the pin is about the script. the trust ladder admits it,
    /// which is the whole of this package: without the manifest field it would pin on an EMPTY
    /// module hash and an edit to the script would never un-pin the layer.
    #[test]
    fn a_script_layer_pins_on_its_script_and_an_edit_moves_the_bytes_the_pin_holds() {
        let t = TempDir::new("script-pin");
        let dir = script_layer(
            &t.path().join("project/.letibot/plugins"),
            "fix",
            "fix.js",
            b"v1\n",
        );
        let book = Trust::in_memory();
        let mut s = stack(&t);
        s.set_trust(book.clone());
        let pin = book.pin("fix", &dir).expect("a pin");
        assert_eq!(pin.module, crate::sha256_hex(b"v1\n"));
        s.refresh_trust();
        assert!(s.layer("p1").unwrap().trusted, "pinned bytes run freely");

        std::fs::write(dir.join("fix.js"), b"v2\n").expect("an edit");
        let pending = s.poll_changes();
        assert_eq!(pending.len(), 1, "the script moved");
        assert_ne!(
            pending[0].sha256, pin.module,
            "what is on disk is no longer what was pinned"
        );
        assert!(
            !book.standing(&dir, &pending[0].sha256, &pin.manifest).free,
            "the pin is a statement about the old text"
        );
    }

    // -----------------------------------------------------------------------------------------
    // The trust ladder - a built-in is free, a placed layer needs a pin, and a pin is about bytes
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_builtin_is_free_and_a_placed_layer_is_not_until_the_book_pins_its_bytes() {
        let t = TempDir::new("trust-arms");
        layer(
            &t.path().join("project/.letibot/plugins"),
            "one",
            &["pane_dashboard"],
            Act::answer(json!({"from": "project"})),
        );
        let mut s = stack(&t);
        s.register_builtin_bytes(
            "builtin",
            "name = \"builtin\"\npurpose = \"p\"\nhooks = [\"pane_builtin\"]\ncapabilities = []\ncontract = 1\n",
            &script(&[("pane_builtin", Act::answer(json!({"from": "binary"})))]),
        )
        .expect("a built-in registers");
        let book = Trust::in_memory();
        s.set_trust(book.clone());

        // Arm 1: the binary's own bytes, free without a pin.
        assert!(s.layer("b-builtin").unwrap().trusted);
        assert!(s.layer("b-builtin").unwrap().pin.is_none());
        // Arm 2: a placed layer is not, and says so by name.
        let placed = s.layer("p1").expect("the project layer");
        assert!(!placed.trusted, "a placed layer needs a pin");
        let said = placed.untrusted_sentence();
        assert!(said.contains("`one`"), "{said}");
        assert!(said.contains("not pinned"), "{said}");

        // Arm 2, the other half: pinning the bytes it has now makes it free.
        let pin = book
            .pin("one", &t.path().join("project/.letibot/plugins/one"))
            .expect("a pin");
        assert!(s.refresh_trust().is_empty(), "nothing was pinned before");
        let placed = s.layer("p1").expect("the project layer");
        assert!(placed.trusted, "a pinned layer runs freely");
        assert_eq!(
            placed.pin.as_ref().map(|p| p.module.as_str()),
            Some(pin.module.as_str())
        );
    }

    #[test]
    fn a_module_rebuilt_after_its_pin_un_pins_the_layer_and_the_stack_says_so_once() {
        let t = TempDir::new("trust-broken");
        let root = t.path().join("project/.letibot/plugins");
        layer(
            &root,
            "one",
            &["pane_dashboard"],
            Act::answer(json!({"from": "v1"})),
        );
        let book = Trust::in_memory();
        let mut s = stack(&t);
        s.set_trust(book.clone());
        let pin = book.pin("one", &root.join("one")).expect("a pin");
        s.refresh_trust();
        assert!(s.layer("p1").unwrap().trusted);

        // The module is rebuilt - the shape this rule exists for.
        write_wasm(
            &root.join("one"),
            &["pane_dashboard"],
            Act::answer(json!({"from": "v2, rebuilt"})),
        );
        let pending = s.poll_changes().into_iter().next().expect("a candidate");
        s.apply(pending).expect("the rebuilt layer applies");
        let placed = s.layer("p1").unwrap();
        assert!(!placed.trusted, "the pin was about the old bytes");
        let said = placed.untrusted_sentence();
        assert!(said.contains(&pin.module), "the pinned hash: {said}");
        assert!(
            said.contains(&placed.sha256),
            "and what is there now: {said}"
        );
        let rebuilt = placed.sha256.clone();

        // Said once, by name: the news a host turns into a `plugin_untrusted` warning.
        let news = s.refresh_trust();
        assert_eq!(news.len(), 1, "{news:?}");
        assert_eq!(news[0].layer, "p1");
        assert_eq!(news[0].pin.module, pin.module);
        assert_eq!(news[0].module, rebuilt);
        assert!(
            s.refresh_trust().is_empty(),
            "a poll every two seconds is not news twice"
        );
    }

    #[test]
    fn a_layer_named_by_its_name_is_the_layer_a_person_meant() {
        let t = TempDir::new("trust-named");
        layer(
            &t.path().join("project/.letibot/plugins"),
            "one",
            &["pane_dashboard"],
            Act::answer(json!({})),
        );
        let s = stack(&t);
        assert_eq!(s.layer_named("one").map(|l| l.id.as_str()), Some("p1"));
        assert_eq!(s.layer_named("p1").map(|l| l.id.as_str()), Some("p1"));
        assert!(s.layer_named("nope").is_none());
        assert_eq!(s.placed().len(), 1);
    }

    /// A layer directory whose manifest declares one tool.
    fn tool_layer(root: &Path, name: &str, tool: &str, caps: &[&str]) {
        let dir = tool_manifest(root, name, tool, caps);
        write_wasm(&dir, &[&format!("tool_{tool}")], Act::answer(json!({})));
    }

    /// The same directory with the manifest and no module - the documented install of a
    /// built-in's tool, where the module that answers is one the host already carries.
    fn tool_manifest(root: &Path, name: &str, tool: &str, caps: &[&str]) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("a layer directory");
        let caps: Vec<String> = caps.iter().map(|c| format!("\"{c}\"")).collect();
        std::fs::write(
            dir.join(MANIFEST),
            format!(
                "name = \"{name}\"\npurpose = \"p\"\nhooks = [\"tool_{tool}\"]\n\
                 capabilities = [{}]\ncontract = 1\n\n[[tools]]\nname = \"{tool}\"\n\
                 description = \"what {name} offers\"\n\nschema = {{ type = \"object\" }}\n",
                caps.join(", ")
            ),
        )
        .expect("plugin.toml");
        dir
    }

    #[test]
    fn declared_tools_are_read_without_loading_a_module_and_in_precedence_order() {
        let t = TempDir::new("declared-tools");
        // Two layers in the project scope - one declaring a tool, one that is only a pane - and a
        // session layer that overrides the project's tool by name.
        tool_layer(
            &t.path().join("project/.letibot/plugins"),
            "from-project",
            "look",
            &["read"],
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "a-pane",
            &["pane_dashboard"],
            Act::answer(json!({})),
        );
        tool_layer(
            &t.path().join("session"),
            "from-session",
            "look",
            &["read:~/.claude/projects"],
        );
        let declared = Stack::declared_tools(
            Some(&t.path().join("session")),
            Some(&t.path().join("project")),
            Some(&t.path().join("user")),
        );
        // The session layer first: it is the one on top, so it is the declaration the model is
        // offered. The pane layer contributes nothing, and nothing was compiled to find that out.
        assert_eq!(declared.len(), 2, "{declared:?}");
        assert_eq!(declared[0].layer, "from-session");
        assert_eq!(declared[0].scope, Scope::Session);
        assert_eq!(declared[0].tool.name, "look");
        assert_eq!(
            declared[0].capabilities,
            vec![Capability::ReadPath("~/.claude/projects".into())]
        );
        assert_eq!(declared[1].layer, "from-project");
        assert_eq!(declared[1].scope, Scope::Project);
        assert_eq!(declared[1].tool.description, "what from-project offers");
    }

    #[test]
    fn a_layer_whose_manifest_does_not_parse_is_skipped_by_the_scanner() {
        // `discover` reports it; this scanner is the prompt's half and has no failure list.
        let t = TempDir::new("declared-broken");
        let dir = t.path().join("project/.letibot/plugins/broken");
        std::fs::create_dir_all(&dir).expect("a directory");
        std::fs::write(dir.join(MANIFEST), "name = \"x\"\n").expect("a bad manifest");
        let declared = Stack::declared_tools(None, Some(&t.path().join("project")), None);
        assert!(declared.is_empty(), "{declared:?}");
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
    fn a_disabled_disk_layer_stays_off_after_a_restart_in_every_scope() {
        let t = TempDir::new("disabled-restart");
        for scope in ["session", "project/.letibot/plugins", "user/plugins"] {
            layer(
                &t.path().join(scope),
                "a",
                &["pane_dashboard"],
                Act::answer(json!({})),
            );
        }
        let mut s = stack(&t);
        for id in ["s1", "p1", "u1"] {
            s.set_disabled(id, true).unwrap();
        }
        let s = stack(&t);
        for id in ["s1", "p1", "u1"] {
            assert!(s.layer(id).unwrap().disabled, "{id} came back on");
        }
        let mut s = s;
        s.set_disabled("p1", false).unwrap();
        let s = stack(&t);
        assert!(!s.layer("p1").unwrap().disabled);
        assert!(s.layer("s1").unwrap().disabled);
    }

    #[test]
    fn a_mask_is_not_saved_and_a_deleted_layer_takes_its_mark_along() {
        let t = TempDir::new("disabled-mask");
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::answer(json!({})),
        );
        let mut s = stack(&t);
        s.mask("s1", true).unwrap();
        assert!(!stack(&t).layer("s1").unwrap().disabled);
        s.set_disabled("s1", true).unwrap();
        std::fs::remove_dir_all(t.path().join("session").join("a")).unwrap();
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::answer(json!({})),
        );
        assert!(!stack(&t).layer("s1").unwrap().disabled);
    }

    #[test]
    fn a_disabled_builtin_stays_off_after_a_restart() {
        let t = TempDir::new("disabled-builtin");
        let state = t.path().join("session").join(".disabled-builtins");
        let register = |s: &mut Stack| {
            let manifest = Manifest::parse(
                "name = \"b\"\npurpose = \"p\"\nhooks = [\"pane_dashboard\"]\n\
                 capabilities = []\ncontract = 1\n",
            )
            .unwrap();
            s.register_builtin(
                Layer {
                    id: String::new(),
                    scope: Scope::BuiltIn,
                    dir: PathBuf::new(),
                    manifest,
                    sha256: String::new(),
                    manifest_sha256: String::new(),
                    disabled: false,
                    trusted: true,
                    pin: None,
                },
                Box::new(Fake::one("pane_dashboard", Act::answer(json!({})))),
            );
        };
        let mut s = stack(&t);
        s.keep_builtin_state(state.clone());
        register(&mut s);
        s.set_disabled("b-b", true).unwrap();
        let mut s = stack(&t);
        s.keep_builtin_state(state.clone());
        register(&mut s);
        assert!(s.layer("b-b").unwrap().disabled);
        s.set_disabled("b-b", false).unwrap();
        let mut s = stack(&t);
        s.keep_builtin_state(state);
        register(&mut s);
        assert!(!s.layer("b-b").unwrap().disabled);
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
                    manifest_sha256: String::new(),
                    disabled: false,
                    trusted: false,
                    pin: None,
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

    /// A built-in the binary carries as BYTES, at the bottom of the stack: no directory, a manifest
    /// that had to parse, a module that had to be admitted, and an id built from its name. The scope
    /// above it still wins, because a built-in is a default and not an exception.
    #[test]
    fn a_builtin_from_bytes_sits_at_the_bottom_and_obeys_the_scopes_above_it() {
        let t = TempDir::new("builtin-bytes");
        layer(
            &t.path().join("session"),
            "a",
            &["pane_sessions"],
            Act::answer(json!({"from": "session"})),
        );
        let mut s = stack(&t);
        let manifest = "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
                        capabilities = [\"read\"]\ncontract = 1\n[contracts]\n\
                        pane_sessions = 3\n";
        s.register_builtin_bytes(
            "sessions",
            manifest,
            &script(&[("pane_sessions", Act::answer(json!({"from": "builtin"})))]),
        )
        .expect("a built-in registers");
        assert_eq!(
            s.layers().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["s1", "b-sessions"],
        );
        // It is the bottom layer: no directory, and the operator's by definition.
        let builtin = s.layer("b-sessions").expect("the built-in");
        assert!(builtin.dir.as_os_str().is_empty());
        assert_eq!(builtin.scope, Scope::BuiltIn);
        assert!(builtin.trusted);
        assert_eq!(
            builtin.sha256.len(),
            64,
            "the bytes are hashed like any layer's"
        );
        // With the session layer above it, the session answers.
        assert_eq!(
            answer_of(&s.call("pane_sessions", &json!({}))),
            json!({"from": "session"})
        );
        s.set_disabled("s1", true).unwrap();
        let r = s.call("pane_sessions", &json!({}));
        assert_eq!(r.by.as_deref(), Some("b-sessions"));
        assert_eq!(answer_of(&r), json!({"from": "builtin"}));
    }

    /// A loader with no memory path refuses a built-in, and the stack says so - a fake engine
    /// must not be able to pretend it loaded something it never saw.
    #[test]
    fn a_loader_that_cannot_load_bytes_answers_a_sentence_and_records_it() {
        struct FilesOnly;
        impl Loader for FilesOnly {
            fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
                Err(format!("no engine here: {}", wasm.display()))
            }
        }
        let mut s = Stack::discover(None, None, None, Arc::new(FilesOnly));
        let manifest = "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
                        capabilities = []\ncontract = 1\n[contracts]\n\
                        pane_sessions = 3\n";
        let why = s
            .register_builtin_bytes("sessions", manifest, b"not a module")
            .expect_err("a loader without `load_bytes` cannot register a built-in");
        assert!(why.contains("load_bytes"), "{why}");
        assert!(s.layers().is_empty());
        let failure = s.failures().first().expect("the refusal is recorded");
        assert_eq!(failure.layer, "b-sessions", "the built-in is named");
        assert_eq!(failure.scope, Scope::BuiltIn);
    }

    /// A built-in whose manifest does not admit its module is refused by NAME, not left on the
    /// stack half-registered - the same rule discovery follows, and the one the stack view draws
    /// as *installed and exploding* rather than as absent.
    #[test]
    fn a_builtin_whose_exports_its_manifest_does_not_declare_is_refused() {
        let t = TempDir::new("builtin-admit");
        let mut s = stack(&t);
        let manifest = "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_other\"]\n\
                        capabilities = []\ncontract = 1\n";
        let why = s
            .register_builtin_bytes(
                "sessions",
                manifest,
                &script(&[("pane_sessions", Act::answer(json!({})))]),
            )
            .expect_err("the manifest and the exports disagree");
        assert!(why.contains("pane_sessions"), "{why}");
        assert!(s.layers().is_empty(), "nothing half-registered");
    }

    /// A layer written for the shape before a move is refused at load, by name, and the layer
    /// below it answers. No wire can tell the two shapes apart, so the manifest is the witness.
    #[test]
    fn a_layer_written_for_the_old_shape_is_refused_and_the_layer_below_answers() {
        let t = TempDir::new("old-shape");
        let dir = t.path().join("session").join("sessions");
        std::fs::create_dir_all(&dir).expect("a layer directory");
        // What a person wrote before the session list became a tree: the hook, and no shape.
        std::fs::write(
            dir.join(MANIFEST),
            "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
             capabilities = []\ncontract = 1\n",
        )
        .expect("plugin.toml");
        write_wasm(
            &dir,
            &["pane_sessions"],
            Act::answer(json!({"from": "groups"})),
        );
        let mut s = stack(&t);
        s.register_builtin_bytes(
            "sessions",
            "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
             capabilities = []\ncontract = 1\n[contracts]\npane_sessions = 3\n",
            &script(&[("pane_sessions", Act::answer(json!({"from": "tree"})))]),
        )
        .expect("the built-in registers");

        // The refusal names the layer, the version it declares and the one this host speaks.
        let why = s
            .failures()
            .iter()
            .find(|f| f.layer == "s1")
            .map(|f| f.why.clone())
            .expect("the old-shape layer is recorded as refused");
        assert!(why.contains("`sessions`"), "{why}");
        assert!(why.contains("contract 1"), "{why}");
        assert!(why.contains("this host speaks 3"), "{why}");
        assert!(
            why.contains("pane_sessions = 3"),
            "and the line that fixes it: {why}"
        );

        // And it never answers: the built-in below it draws the list.
        let r = s.call("pane_sessions", &json!({}));
        assert_eq!(r.by.as_deref(), Some("b-sessions"), "{:?}", r.failures);
        assert_eq!(answer_of(&r), json!({"from": "tree"}));
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
    fn an_ask_below_round_trips_the_lower_answer_into_the_layer_that_asked() {
        let t = TempDir::new("below");
        // The session layer changes one field of whatever answers below: `{"below":…}` on the
        // first call, and on the second it answers with what it was handed.
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::ask_below(json!({"v": 1, "edited": true})),
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

    /// A pane's `look` is an answer, not the stack's ask-below. A pane that asks the host to read a
    /// file answers a document with a top-level `look`; a stack that read that as *ask the layer
    /// below* drew nothing at all. `below` is the marker now, and `look` belongs to the pane.
    #[test]
    fn a_pane_answer_carrying_c3s_look_answers_without_asking_below() {
        let t = TempDir::new("pane-look");
        let asks = json!({
            "title": "plan",
            "look": {"read": ".letibot/status.json"},
            "every_s": 0,
        });
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::answer(asks.clone()),
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["pane_dashboard"],
            Act::answer(json!({"title": "never asked"})),
        );
        let r = stack(&t).call("pane_dashboard", &json!({"v": 1}));
        assert_eq!(r.by.as_deref(), Some("s1"), "the top layer answers");
        assert_eq!(answer_of(&r), asks, "and it is the pane's own document");
        assert!(
            r.failures.is_empty(),
            "a pane's look is not a broken ask-below: {:?}",
            r.failures
        );
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
    fn a_second_ask_from_one_layer_is_a_recorded_failure_not_a_loop() {
        let t = TempDir::new("two-asks");
        // This fake answers `{"below":…}` on every call, including the one carrying `below`.
        layer(
            &t.path().join("session"),
            "a",
            &["pane_dashboard"],
            Act::always_ask(json!({"v": 1})),
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
            r.failures.iter().any(|f| f.why.contains("second `below`")),
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

    /// A manifest and no module is a declaration, not a failure - the documented install of a
    /// built-in's tool. Two failures here would be published as `plugin_layer` warnings on the one
    /// code that means *this layer is exploding*.
    #[test]
    fn a_manifest_with_no_module_is_a_declaration_and_not_a_failure() {
        let t = TempDir::new("declared-only");
        let session = t.path().join("session");
        let dir = tool_manifest(
            &session,
            "from-builtin",
            "look",
            &["read:~/.claude/projects"],
        );
        let mut s = stack(&t);
        assert!(
            s.failures().is_empty(),
            "a correct install must not look broken: {:?}",
            s.failures()
        );
        assert_eq!(
            s.layers().len(),
            1,
            "it is a layer: the model is offered its tool"
        );
        assert_eq!(s.layers()[0].id, "s1");
        assert_eq!(s.layers()[0].sha256, "", "there are no bytes to hash");
        // Nothing answers it from here - the module that does is one the host puts below.
        assert_eq!(s.call("tool_look", &json!({})).answer, None);
        assert!(
            s.failures().is_empty(),
            "and resolving it is not a failure either"
        );
        assert!(
            s.poll_changes().is_empty(),
            "a poll of the same directory is silent"
        );

        // A module written beside the manifest is a change the poll sees, so a declaration is not
        // a layer that can never answer.
        write_wasm(&dir, &["tool_look"], Act::answer(json!({"ok": true})));
        let pending = s.poll_changes();
        assert_eq!(pending.len(), 1, "the module that arrived is a change");
        assert_eq!(pending[0].layer, "s1", "for the layer that already exists");
        s.apply(pending.into_iter().next().unwrap()).unwrap();
        assert_eq!(
            answer_of(&s.call("tool_look", &json!({}))),
            json!({"ok": true})
        );
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

    /// A guard layer that traps, returns malformed text or asks below twice refuses the call and
    /// names itself. An advisory hook falls through to the layer below as before.
    #[test]
    fn a_failed_guard_refuses_and_a_failed_advisory_layer_falls_through() {
        for (tag, act) in [
            ("trap", Act::trap("out of fuel")),
            ("malformed", Act::malformed("not json")),
            ("twice", Act::always_ask(json!({"v": 1}))),
        ] {
            let t = TempDir::new(&format!("guard-{tag}"));
            layer(
                &t.path().join("session"),
                "a",
                &["on_tool_call", "notice_fired"],
                act,
            );
            layer(
                &t.path().join("project/.letibot/plugins"),
                "b",
                &["on_tool_call", "notice_fired"],
                Act::answer(json!({"from": "below"})),
            );
            let s = stack(&t);
            let r = s.call("on_tool_call", &json!({}));
            let (by, why) = r.refused.clone().unwrap_or_else(|| panic!("{tag}: {r:?}"));
            assert_eq!(by, "s1", "{tag}");
            assert!(!why.is_empty(), "{tag}");
            assert_eq!(
                r.answer, None,
                "{tag}: the layer below does not answer for a failed guard"
            );
        }
        let t = TempDir::new("advisory-trap");
        layer(
            &t.path().join("session"),
            "a",
            &["notice_fired"],
            Act::trap("boom"),
        );
        layer(
            &t.path().join("project/.letibot/plugins"),
            "b",
            &["notice_fired"],
            Act::answer(json!({"from": "below"})),
        );
        let r = stack(&t).call("notice_fired", &json!({}));
        assert_eq!(r.refused, None);
        assert_eq!(r.answer, Some(json!({"from": "below"})));
    }
}
