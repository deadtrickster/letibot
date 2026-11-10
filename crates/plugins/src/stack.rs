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
    /// Whether this layer may run freely: true for a built-in, and for a layer whose module and
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

/// A pin whose bytes moved: the layer, what was pinned, and what is on disk now. Drained by the
/// host, which says it once (`plugin_untrusted`): a layer that quietly stopped being yours is what
/// the ladder exists to make impossible to miss.
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

/// Hooks whose failure must stop what they guard rather than let it through. The gate's floor is
/// one: a policy that cannot answer has not said the call is fine, and the gate reads its failure
/// as the strictest tier.
const FAIL_CLOSED: &[&str] = &["on_tool_call", letibot_plugin::adjudicate::HOOK];

/// Hooks a session's own layer may not answer at all, whatever the manifest declared: reviewed
/// policy about what may run is a project's, the operator's or a built-in's, never that of the agent
/// it governs, and so is a review of that agent's own branch.
const NOT_A_SESSIONS: &[&str] = &[
    letibot_plugin::adjudicate::HOOK,
    letibot_plugin::review::BRIEF,
    letibot_plugin::review::VERDICT,
];

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
    /// hash the stack already holds. The answer is what is new, said once by the caller.
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
            // The name `declared_tools` carries is the layer's directory name, not its path -
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

    /// Register a built-in the binary carries as bytes, read into the bottom of the stack. The
    /// manifest is parsed from its text and the module comes from memory, so it goes through the
    /// same doors a layer on disk does. A failure is answered and recorded, as discovery does.
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
        // `register_builtin` records its own refusals; answer the one that belongs to this call
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
        // The module first, because `admit_for` compares the exports to the declared hooks, and the
        // standing at the same time because its chat-feed rule is decided by the pin.
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
        let manifest_sha256 = sha256_file(&dir.join(MANIFEST)).unwrap_or_else(|_| String::new());
        // Decided against the bytes that are there NOW: a rebuild is a different layer to the pin
        // book, and this is the standing the scope rules and `Layer::trusted` are both decided by.
        let (trusted, pin) = self.standing_of(&dir, &seen.sha256, &manifest_sha256);
        // `admit` is the contract and the exports; `admit_for` adds the rules about the LAYER - a
        // session may not answer the gate's floor or a review of its own branch, and the `chat` feed
        // is a pinned layer's or a built-in's. This is the door every new or rebuilt layer takes.
        manifest
            .admit_for(&hooks, c.scope, trusted)
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
                self.layers[i].manifest_sha256 = manifest_sha256;
                self.layers[i].trusted = trusted;
                self.layers[i].pin = pin;
                self.wired.insert(id, wired);
            }
            None => {
                let id = self.next_id(c.scope);
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
            // `plugin_layer` warnings for a correct install of a built-in's tool, and there is
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
            // Reviewed policy is nobody's own layer: a session that could answer this hook could
            // raise its own floor to `auto`, so it never reaches a module at all.
            if layer.scope == Scope::Session && NOT_A_SESSIONS.contains(&walk.hook) {
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
#[cfg(test)]
mod tests;
