//! The loader and installer: the doors a session's stack is built at, the binary's own layers each
//! door seats, the hot-reload clock, and the `plugin` tool's view of what is in force.
//! Admission itself is `letibot_plugins::admit`'s; this is where the daemon decides who is asked.

use super::*;

impl PluginHost {
    /// Discover a session's layers, from the three scopes: the session's own dir, the project's
    /// `.letibot/plugins` (whose files a `read` look resolves against) and the user's. Any of them
    /// may be absent, which is an empty scope and not an error.
    pub fn discover(
        session: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_dir: Option<PathBuf>,
        user_dir: Option<PathBuf>,
        loader: Arc<dyn Loader>,
        builder: Arc<dyn Builder>,
    ) -> PluginHost {
        let workspace = workspace.into();
        let checkout = feed::checkout_of(&workspace);
        // The same admission path the head and the bridge take. No book here: the daemon opens it
        // with the session, so `with_trust` decides the placed layers a moment later.
        let mut stack = letibot_plugins::admit(
            letibot_plugins::Scopes {
                session: session_dir.as_deref(),
                workspace: &workspace,
                user: user_dir.as_deref(),
            },
            loader,
            None,
        )
        .stack;
        // Beside the session's layers, not in one: the dir scan only takes subdirectories.
        if let Some(dir) = session_dir.as_deref() {
            stack.keep_builtin_state(dir.join(".disabled-builtins"));
        }
        PluginHost {
            session: session.into(),
            workspace,
            checkout,
            session_dir,
            inner: Mutex::new(Inner {
                stack: Arc::new(stack),
                rec: Recorder::new(),
                panes: BTreeMap::new(),
                sizes: BTreeMap::new(),
                gating: BTreeSet::new(),
                said: BTreeSet::new(),
                notices: Vec::new(),
                poll_at: Instant::now() + POLL_EVERY,
                broken_pins: Vec::new(),
                builtin_asked: BTreeSet::new(),
            }),
            builder,
            sessions: None,
            room: Mutex::new(None),
            github: None,
            install_builtin_panes: false,
            builtins_wanted: std::sync::atomic::AtomicBool::new(false),
            builtins_started: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Hand this host the daemon's session list, so a pane declaring the `sessions` feed is given
    /// one. A builder and not a constructor argument: the list is the daemon's fact, not a layer's.
    pub fn with_sessions(mut self, source: Arc<dyn feed::SessionFeed>) -> PluginHost {
        self.sessions = Some(source);
        self
    }

    /// Hand this host the session's own connection to the box's room, so a pane declaring the
    /// `room` feed is given one and its four controls have somewhere to go.
    pub fn with_room(self, source: Arc<dyn room_feed::RoomFeed>) -> PluginHost {
        *self.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
        self
    }

    /// Hand this host the daemon's GitHub reviewer seat, so a pane declaring the `github` feed is
    /// drawn from it: `letibot attach github <scope>` is what puts a scope there.
    pub fn with_github(mut self, source: Arc<dyn feed::GithubFeed>) -> PluginHost {
        self.github = Some(source);
        self
    }

    /// [`PluginHost::discover`] with the dirs this box's conventions give it: the session scope
    /// `<data dir>/sessions/<id>/plugins` and `$XDG_CONFIG_HOME/letibot`. An absent scope is empty.
    pub fn for_session(
        session: &str,
        workspace: &Path,
        loader: Arc<dyn Loader>,
        builder: Arc<dyn Builder>,
    ) -> PluginHost {
        let session_dir = session_plugins_dir(session);
        PluginHost::discover(
            session,
            workspace,
            session_dir,
            user_config_dir(),
            loader,
            builder,
        )
        // The trust book, read once here and re-read on the poll: a placed layer is free only when
        // a person recorded a hash of these exact bytes. No book (a test's stack) pins nothing.
        .with_trust(letibot_plugins::Trust::open_default())
    }

    /// Hand this host the trust book: which placed layers a person has pinned, and which changed
    /// since. A pin that broke while no daemon was running is news, and this is where it arrives
    /// the moved pins are queued, and the first [`PluginHost::tick`] says them.
    pub fn with_trust(self, trust: Arc<letibot_plugins::Trust>) -> PluginHost {
        let news = self.lock().stack_mut().set_trust(trust);
        if !news.is_empty() {
            self.lock().broken_pins.extend(news);
        }
        self
    }

    /// What the trust book says about a layer this host holds - by id, manifest name, or the
    /// directory it sits in. A clone because the caller holds the host's lock across its call.
    pub fn layer(&self, what: &str) -> Option<letibot_plugins::Layer> {
        self.lock().stack.layer_named(what).cloned()
    }

    /// [`PluginHost::layer`] with the scope it must be: two scopes can hold layers of one name (a
    /// session fix and the project layer it was promoted from), so a name alone is two answers to
    /// *is this pinned*. The fallback is the name-only lookup, for a layer that has moved scope.
    pub fn layer_in(&self, what: &str, scope: Scope) -> Option<letibot_plugins::Layer> {
        let inner = self.lock();
        inner
            .stack
            .layers()
            .iter()
            .find(|l| {
                l.scope == scope
                    && (l.id == what
                        || l.manifest.name == what
                        || l.dir.file_name().is_some_and(|n| n == what))
            })
            .or_else(|| inner.stack.layer_named(what))
            .cloned()
    }

    /// This host as the daemon uses it: rano loads the modules and cargo builds them. A built-in
    /// that declares a tool is seated only for a session that declared it; its panes are installed
    /// at the door a head comes through.
    pub fn production(session: &str, workspace: &Path) -> PluginHost {
        let mut host = PluginHost::for_session(
            session,
            workspace,
            Arc::new(rano::RanoLoader),
            Arc::new(CargoBuilder),
        );
        host.install_builtin_panes = true;
        host.install_builtin_tools(&crate::plugins::declared_tools(session, workspace));
        host.install_builtin_nags();
        // The third door's other half: a built-in whose hook is neither a pane nor a tool decides
        // something on every session, and the harness's own decision is a layer only if it is on
        // the stack.
        host.install_builtin_deciders();
        host
    }

    /// The host a spawned child gets: [`PluginHost::production`] for the child's own dirs and workspace,
    /// with what [`Inherited::of`] says it takes from its parent. Without it a reviewer's
    /// `review_verdict` cannot be guarded, because the child's stack has nothing to ask.
    pub fn for_child(session: &str, workspace: &Path, inherited: Inherited) -> PluginHost {
        PluginHost::production(session, workspace).with_inherited(inherited)
    }

    /// Take a parent's pinned session layers onto this stack. A builder rather than an argument of
    /// [`PluginHost::discover`] so a test's host inherits by the same code the daemon's child does.
    pub fn with_inherited(self, inherited: Inherited) -> PluginHost {
        for dir in inherited.dirs {
            // One guard for both steps: `lock` is not reentrant, so holding it across a second
            // `lock()` in the same statement would deadlock on the first layer to inherit.
            let applied = {
                let mut inner = self.lock();
                match inner.stack.candidate(&dir, Scope::Session) {
                    Ok(cand) => inner.stack_mut().apply(cand),
                    Err(why) => Err(why),
                }
            };
            if let Err(why) = applied {
                // Said once, to this child's own model: the parent's pin is on bytes this child
                // could not load, and the same line at every call is noise a model learns to skip.
                self.say_once(format!(
                    "a plugin layer inherited from the session that spawned this one could not be \
                     loaded, so it is not answering here: {why}"
                ));
            }
        }
        self
    }

    /// Put the binary's built-in layer for `hook` at the bottom of this stack, once. It is the third
    /// door, after a declared tool's and a head's pane's: a hook the daemon asks on a session's behalf is
    /// answered by a built-in where nobody has words, installed on the first ask because a compile costs.
    pub(super) fn install_builtin_for(&self, hub: &Arc<Hub>, hook: &str) {
        if !self.install_builtin_panes {
            return;
        }
        {
            let mut inner = self.lock();
            if inner.builtin_asked.contains(hook) {
                return;
            }
            inner.builtin_asked.insert(hook.to_string());
        }
        self.install_where(Some(hub), |manifest| {
            // A built-in that declares a tool or a pane belongs to that door: this one is for the
            // hooks the daemon asks itself, and a layer seated for a hook nobody called is cost
            // with no reader.
            let has_a_door = manifest
                .tools
                .iter()
                .any(|t| manifest.hooks.contains(&format!("tool_{}", t.name)))
                || manifest.hooks.iter().any(|h| h.starts_with("pane_"));
            (!has_a_door && manifest.hooks.iter().any(|h| h.as_str() == hook)).then(Vec::new)
        });
    }

    /// Put the built-in layers a session declared a tool from at the bottom of this stack, matched
    /// on the tool's name. A project's ask can then be a manifest with no module: the built-in is
    /// the module that answers.
    fn install_builtin_tools(&self, declared: &[DeclaredTool]) {
        if declared.is_empty() {
            return;
        }
        self.install_where(None, |manifest| {
            manifest
                .tools
                .iter()
                .any(|t| declared.iter().any(|d| d.tool.name == t.name))
                .then(Vec::new)
        });
    }

    /// Put the binary's built-in layers that answer `nag_` at the bottom of this stack, so the
    /// plan-check's default is a layer rather than a branch: the answer is `b-todo`'s, and an operator's
    /// layer above it is the project's own. Without this the hook would have nothing to ask.
    fn install_builtin_nags(&self) {
        self.install_where(None, |manifest| {
            manifest
                .hooks
                .iter()
                .any(|h| h.strip_prefix("nag_").is_some_and(|n| !n.is_empty()))
                .then(Vec::new)
        });
    }

    /// Put the binary's own `steer_`/`compact_` decisions on this stack: the built-in layers whose hooks
    /// are neither a pane (a head's door) nor a tool (a project's declaration), so they would otherwise
    /// be installed at no door at all. A `nag_` built-in comes through [`PluginHost::install_builtin_nags`].
    fn install_builtin_deciders(&self) {
        self.install_where(None, |manifest| {
            let decides = manifest.hooks.iter().any(|h| {
                h.starts_with(letibot_plugin::steer::PREFIX)
                    || h.starts_with(letibot_plugin::compact::PREFIX)
            });
            // A layer that already answers the hook is the one that decides: the built-in belongs
            // under it, and a stack that put it above would silently overrule the project.
            decides.then(|| manifest.hooks.clone())
        });
    }

    /// Install the binary's built-in pane layers the first time a head comes through a door that needs
    /// them: a module costs seconds to compile, and a session nobody attached to must not pay for one.
    pub(super) fn ensure_builtin_panes(&self) {
        if !self.install_builtin_panes {
            return;
        }
        self.install_where(None, |manifest| {
            let panes: Vec<String> = manifest
                .hooks
                .iter()
                .filter(|h| h.strip_prefix("pane_").is_some_and(|n| !n.is_empty()))
                .cloned()
                .collect();
            // A layer that already answers the hook is the one that draws it - ours would sit below
            // it and never be called, and a project that shipped its own pane has said which it
            // wants. The layer itself, installed for a declared tool, is that layer too.
            (!panes.is_empty()).then_some(panes)
        });
    }

    /// The one loop behind every door above: walk the built-in table, take what `wanted` names, and
    /// put it under the stack's own layers.
    ///
    /// `wanted` answers what makes a built-in already answered, and `None` for *not this door's*: one
    /// rule, so a sixth door cannot forget the guard, the answered check or the failure line.
    fn install_where(
        &self,
        hub: Option<&Arc<Hub>>,
        wanted: impl Fn(&Manifest) -> Option<Vec<String>>,
    ) -> usize {
        let mut installed = 0;
        for builtin in letibot_builtins::table() {
            let Ok(manifest) = Manifest::parse(builtin.manifest) else {
                continue;
            };
            let Some(hooks) = wanted(&manifest) else {
                continue;
            };
            if !hooks.is_empty() && self.answered(&hooks) {
                continue;
            }
            // Compiled outside the host's lock: a compile costs seconds, and every pane, hook and
            // turn of this session takes that lock.
            let stack = Arc::clone(&self.lock().stack);
            let prepared = stack.prepare_builtin(builtin.name, builtin.manifest, builtin.wasm);
            drop(stack);
            let registered = prepared.and_then(|p| {
                let mut inner = self.lock();
                // Another door may have stacked it while this one compiled.
                let stacked = inner.stack.layers().iter().any(|l| {
                    l.id == p.id()
                        || (!hooks.is_empty()
                            && !l.disabled
                            && l.manifest.hooks.iter().any(|h| hooks.contains(h)))
                });
                if stacked {
                    return Ok(false);
                }
                inner.stack_mut().register_prepared(p).map(|()| true)
            });
            match registered {
                Ok(true) => installed += 1,
                Ok(false) => {}
                Err(why) => self.say_builtin_failed(hub, builtin.name, &why),
            }
        }
        installed
    }

    /// Whether an enabled layer here already answers one of `hooks`.
    fn answered(&self, hooks: &[String]) -> bool {
        let inner = self.lock();
        inner
            .stack
            .layers()
            .iter()
            .any(|l| !l.disabled && l.manifest.hooks.iter().any(|h| hooks.contains(h)))
    }

    /// A built-in that would not load, said once: the hooks it answers fall back to the harness's own,
    /// and while the stack has recorded the failure, only this line names the layer that broke.
    fn say_builtin_failed(&self, hub: Option<&Arc<Hub>>, name: &str, why: &str) {
        let said = format!(
            "the plugin built-in `{name}` did not load, so the hooks it answered fall to the \
             harness's own: {why}"
        );
        if self.say_once(said.clone())
            && let Some(hub) = hub
        {
            hub.warn("plugin_layer", said);
        }
    }

    /// Give a host built before the session had a room its connection, and show the pane to a head
    /// that is already attached.
    pub fn link_room(&self, source: Arc<dyn room_feed::RoomFeed>, hub: &Arc<Hub>) {
        *self.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
        PaneDriver::attach(self, "", hub);
    }

    /// A head has arrived and the built-in panes are wanted, and their compile has not been started.
    pub(super) fn builtins_due(&self) -> bool {
        self.install_builtin_panes
            && self
                .builtins_wanted
                .load(std::sync::atomic::Ordering::SeqCst)
            && !self
                .builtins_started
                .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// When this host next has something to do: the next poll or the earliest pane refresh - or
    /// nothing, for a session with no layers at all.
    pub fn next_at(&self) -> Option<Instant> {
        if self.builtins_due() {
            return Some(Instant::now());
        }
        let inner = self.lock();
        if inner.stack.layers().is_empty() {
            // A session with no layers is not on the clock at all: polling three directories that
            // do not exist every two seconds is a wake with nothing behind it.
            return None;
        }
        inner
            .panes
            .values()
            .filter_map(|p| p.next_at)
            .chain(std::iter::once(inner.poll_at))
            .min()
    }

    /// The daemon's idle pass: hot reload if its clock is up, then every pane whose `every_s` has
    /// come round. Only the cheap half runs here - a stat per layer - and a change starts a thread
    /// because the gate runs `cargo` and a turn must not wait for a compiler.
    pub fn tick(self: Arc<Self>, hub: Arc<Hub>) {
        if self.builtins_due()
            && !self
                .builtins_started
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let me = Arc::clone(&self);
            let ring = Arc::clone(&hub);
            let _ = std::thread::Builder::new()
                .name("plugin-builtins".into())
                .spawn(move || {
                    me.ensure_builtin_panes();
                    me.announce(&ring);
                });
        }
        // The trust book moves under us: a pin taken in the head is in force here at the next
        // tick, and a layer rebuilt after its pin stops being free at once, gate or no gate.
        let broken = {
            let mut inner = self.lock();
            let mut broken = std::mem::take(&mut inner.broken_pins);
            broken.extend(inner.stack_mut().refresh_trust());
            broken
        };
        for pin in broken {
            self.say_broken_pin(&hub, &pin);
        }
        let changed = {
            let mut inner = self.lock();
            if inner.poll_at > Instant::now() {
                false
            } else {
                inner.poll_at = Instant::now() + POLL_EVERY;
                !inner.stack_mut().poll_changes().is_empty()
            }
        };
        if changed {
            let me = Arc::clone(&self);
            let ring = Arc::clone(&hub);
            let _ = std::thread::Builder::new()
                .name("plugin-gate".into())
                .spawn(move || {
                    me.gate_now(&ring);
                });
        }
        self.refresh_due(&hub);
    }

    /// Gate every changed layer now, synchronously - the hot-reload tick minus the thread. The gate
    /// keeps the module it loaded on the candidate, so `Stack::apply` swaps in exactly the bytes
    /// that were replayed; a refusal leaves the stack as it was.
    pub fn gate_now(&self, hub: &Arc<Hub>) -> usize {
        let mut cands = {
            let mut inner = self.lock();
            inner.poll_at = Instant::now() + POLL_EVERY;
            inner.stack_mut().poll_changes()
        };
        self.say_failures_now(hub);
        // A candidate for a directory that is not a layer yet has no id and nothing to replace.
        cands.retain(|c| !c.layer.is_empty());
        let mut applied = 0;
        for cand in cands {
            let id = cand.layer.clone();
            {
                let mut inner = self.lock();
                if !inner.gating.insert(id.clone()) {
                    continue;
                }
            }
            let (stack, rec) = {
                let inner = self.lock();
                (Arc::clone(&inner.stack), inner.rec.clone())
            };
            let report = letibot_plugins::gate(&stack, &rec, &cand, self.builder.as_ref());
            let refusal = if report.passed {
                let mut inner = self.lock();
                inner.stack_mut().apply(cand).err()
            } else {
                report
                    .failure
                    .clone()
                    .or_else(|| Some("the gate passed no reason".to_string()))
            };
            self.lock().gating.remove(&id);
            match refusal {
                Some(why) => self.refused(hub, &id, &why),
                None => {
                    applied += 1;
                    self.refresh_panes(hub);
                }
            }
        }
        applied
    }

    /// What a rebuild did to this layer's pin, as the sentence `plugin load` says: a rebuild that
    /// asks for nothing new keeps the operator's pin instead of costing another `/plugin trust`.
    /// `None` when this layer was never pinned, which is the ordinary case and says nothing.
    fn settle_pin(&self, hub: &Arc<Hub>, name: &str, dir: &Path) -> Option<String> {
        let trust = self.lock().stack.trust().cloned()?;
        match trust.rebuilt(name, dir) {
            Ok(None) => None,
            Ok(Some(letibot_plugins::Rebuilt::Kept(pin))) => {
                // The pin moved with the bytes, so decide the layer now rather than at the next
                // tick: two seconds of "unpinned" is two seconds of a call adjudicated per call.
                let news = self.lock().stack_mut().refresh_trust();
                for broken in news {
                    self.say_broken_pin(hub, &broken);
                }
                Some(format!(
                    "its pin held: `plugin.toml` declares the capabilities that were pinned, so \
                     `{name}` is still pinned over module sha256 {} and nothing asks you to trust \
                     it again.",
                    pin.module
                ))
            }
            Ok(Some(letibot_plugins::Rebuilt::Dropped { new, was })) => Some(if was.is_none() {
                format!(
                    "**it is not pinned any more**: nothing recorded which capabilities it was \
                     pinned with, so a rebuild cannot be judged against them. `/plugin trust \
                     {name}` pins what is there now, and the powers it declares {}.",
                    if new.is_empty() {
                        "are none".to_string()
                    } else {
                        format!("are {}", new.join(", "))
                    }
                )
            } else if new.is_empty() {
                format!(
                    "**it is not pinned any more**: its `plugin.toml` declares a different set of \
                     capabilities than the ones that were pinned, so the review that pin recorded \
                     does not cover this layer. `/plugin trust {name}` pins it again, and every \
                     call is adjudicated until then."
                )
            } else {
                format!(
                    "**it is not pinned any more**: its `plugin.toml` now declares {}, which was \
                     not there when it was pinned — a wider layer is a new review, and the pin \
                     approved the old powers. `/plugin trust {name}` pins it again, and every call \
                     is adjudicated until then.",
                    new.join(", ")
                )
            }),
            Err(why) => Some(format!(
                "its pin could not be re-decided: {why} — `/plugin trust {name}` pins what is \
                 there now."
            )),
        }
    }

    /// A layer was pinned and its bytes moved - said once, by name, to the head's warnings and the
    /// model's notices. `plugin_untrusted` and not `plugin_refused`: nothing was refused and nothing
    /// is broken, but every call it makes is adjudicated again until somebody pins it.
    fn say_broken_pin(&self, hub: &Arc<Hub>, broken: &letibot_plugins::BrokenPin) {
        let said = format!(
            "the plugin layer `{}` ({}) is not pinned any more: its bytes changed after it was \
             pinned. What was pinned is module sha256 {} and manifest sha256 {}; what is on disk \
             now is module sha256 {} and manifest sha256 {}. Every call it makes is adjudicated \
             again until `/plugin trust {}` pins what is there now",
            broken.name,
            broken.scope,
            broken.pin.module,
            broken.pin.manifest,
            broken.module,
            broken.manifest,
            broken.name,
        );
        {
            let mut inner = self.lock();
            if !inner.said.insert(said.clone()) {
                return;
            }
            inner.notices.push(said.clone());
        }
        hub.warn("plugin_untrusted", said);
    }

    /// A candidate was refused: the old layer stays, and both the model (which can fix it) and the
    /// head (so a person sees why the pane did not change) are told.
    fn refused(&self, hub: &Arc<Hub>, layer: &str, why: &str) {
        let said = format!(
            "the plugin layer `{layer}` was refused by the shadow gate, and the version answering \
             now is the one before it: {why}"
        );
        {
            let mut inner = self.lock();
            if !inner.said.insert(said.clone()) {
                return;
            }
            inner.notices.push(said.clone());
        }
        hub.warn("plugin_refused", said);
    }
}

// The `plugin` tool's half of this module: scaffold, build, load, stack, disable/enable, test.
// Every one of them asks the live stack, because a second stack would be a second answer to
// *what is in force*, and a layered design cannot afford two of those.

impl PluginHost {
    /// The layer directory for a plugin `name`, or the refusal that names what IS there.
    pub fn layer_dir(&self, name: &str) -> Result<PathBuf, Refused> {
        let Some(root) = self.session_dir.as_deref() else {
            return Err(Refused::new(
                "this session has no plugin directory",
                "a session layer lives under the letibot data dir \
                 (<data>/sessions/<id>/plugins), and this daemon could not resolve that directory",
            ));
        };
        let dir = root.join(name);
        if dir.join(letibot_plugins::MANIFEST).is_file() {
            return Ok(dir);
        }
        let mut there: Vec<String> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join(letibot_plugins::MANIFEST).is_file())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        there.sort();
        let detail = if there.is_empty() {
            format!(
                "this session has no plugins of its own yet ({}); `plugin scaffold` writes the \
                 first one there",
                root.display()
            )
        } else {
            format!("this session's plugins are: {}", there.join(", "))
        };
        Err(Refused::new(
            format!("there is no plugin `{name}` in this session"),
            detail,
        ))
    }

    /// `plugin build` - the plugin's source compiled for wasm, and the artifact installed as its
    /// `plugin.wasm`. It goes through the gate's own [`Builder`], and the compiler's words come back
    /// verbatim: a summary of an `E0425` is a model editing the wrong line.
    pub fn build(&self, name: &str) -> Result<String, Refused> {
        let dir = self.layer_dir(name)?;
        match self.builder.build(&dir) {
            Ok(log) => {
                let wasm = dir.join(Manifest::module_name(&dir));
                let bytes = std::fs::metadata(&wasm).map(|m| m.len()).unwrap_or(0);
                let mut said = format!("built `{name}` → {} ({bytes} bytes)", wasm.display());
                let log = log.trim();
                if !log.is_empty() {
                    said.push_str("\n\n");
                    said.push_str(log);
                }
                said.push_str(
                    "\n\nnext: `plugin test` to see the view it answers, then `plugin load`.",
                );
                Ok(said)
            }
            Err(e) => {
                // The compiler's output is the report. When cargo could not even start there is
                // no output, and then the reason IS the detail rather than an empty payload.
                let detail = if e.log.trim().is_empty() {
                    e.why.clone()
                } else {
                    e.log
                };
                Err(Refused::new(
                    format!("the build of `{name}` failed"),
                    detail,
                ))
            }
        }
    }

    /// `plugin load` - the shadow gate, then a synthetic call of every pane the candidate exports
    /// then the swap, hot. The gate replays recorded inputs and a new layer has none, so without the
    /// synthetic call a first-time pane would be applied having never run.
    pub fn load(&self, hub: &Arc<Hub>, name: &str) -> Result<String, Refused> {
        let dir = self.layer_dir(name)?;
        let (stack, rec) = {
            let inner = self.lock();
            (Arc::clone(&inner.stack), inner.rec.clone())
        };
        let cand = stack.candidate(&dir, Scope::Session).map_err(|why| {
            Refused::new(
                format!("`{name}` cannot be a layer yet"),
                format!("{why} — `plugin build {name}` produces the module a layer loads"),
            )
        })?;
        let report = letibot_plugins::gate(&stack, &rec, &cand, self.builder.as_ref());
        if !report.passed {
            let why = report
                .failure
                .clone()
                .unwrap_or_else(|| "the gate passed no reason".to_string());
            let mut detail = why.clone();
            let log = report.build_log.trim();
            if !log.is_empty() && !detail.contains(log) {
                detail.push_str("\n\n");
                detail.push_str(log);
            }
            return Err(Refused::new(
                format!("the shadow gate refused `{name}`"),
                detail,
            ));
        }

        // The shadow runs it, not the live stack: the candidate applied to a clone, every recorded
        // input of every pane hook it exports plus one synthetic first frame, each answer parsed
        // and validated as a view this host's keys can drive.
        let mut shadow = (*stack).clone();
        shadow
            .apply(cand.clone())
            .map_err(|why| Refused::new(format!("`{name}` could not be shadowed"), why))?;
        let hooks: Vec<String> = cand
            .manifest
            .hooks
            .iter()
            .filter(|h| letibot_plugins::is_hook_kind(h))
            .cloned()
            .collect();
        if hooks.is_empty() {
            return Err(Refused::new(
                format!("`{name}` answers no hook this host runs"),
                format!(
                    "its plugin.toml declares {:?}, and the hook kinds are the prefixes {} \
                     (`on_tool_call` and `on_tool_result` are the `on_` ones)",
                    cand.manifest.hooks,
                    letibot_plugins::SERVED_KINDS.join(", ")
                ),
            ));
        }
        let mut said = Vec::new();
        for hook in &hooks {
            let mut inputs = rec.inputs(hook).to_vec();
            // A layer nobody has asked has no recorded inputs, so one of its kind's first frames
            // goes too - that is what makes `load` a proof that the layer runs at all.
            inputs.push(synthetic_for(hook, &self.feeds_for(hook)));
            for input in inputs {
                let resolved = shadow.call(hook, &input);
                let wrong: Vec<String> = resolved
                    .failures
                    .iter()
                    .filter(|f| f.layer.is_empty() || shadow.layer(&f.layer).is_some())
                    .map(|f| f.to_string())
                    .collect();
                if !wrong.is_empty() {
                    return Err(Refused::new(
                        format!("`{name}` fails when it is called, and nothing was applied"),
                        format!(
                            "`{hook}` was asked {} and did not answer: {}\n\nthe version that was \
                             answering before is still answering.",
                            describe_any_input(&input),
                            wrong.join("; ")
                        ),
                    ));
                }
                let Some(answer) = resolved.answer else {
                    said.push(format!(
                        "`{hook}` answers nothing for {} — it passes, and no layer below it \
                         answers, so it does nothing",
                        describe_any_input(&input)
                    ));
                    continue;
                };
                let by = resolved.by.clone().unwrap_or_else(|| name.to_string());
                let fuel = shadow.fuel(&by).unwrap_or(0);
                let declared = shadow
                    .layer(&by)
                    .map(|l| l.manifest.actions.clone())
                    .unwrap_or_default();
                match accepted(hook, &answer, &declared, &input) {
                    Ok(what) => said.push(format!(
                        "`{hook}` for {}{} — {what}",
                        describe_any_input(&input),
                        fuel_note(fuel)
                    )),
                    Err(why) => {
                        return Err(Refused::new(
                            format!(
                                "`{name}` answers something the host refuses, so nothing was applied"
                            ),
                            format!(
                                "`{hook}` on {}: {why}\n\nthe answer was: {}",
                                describe_any_input(&input),
                                answer
                            ),
                        ));
                    }
                }
            }
        }

        // What a person is being asked to trust. The hooks are what the layer then answers, and the
        // capabilities are what it may reach, so the card names both rather than a count.
        let answers: Vec<String> = cand.manifest.hooks.clone();
        let wants: Vec<String> = cand
            .manifest
            .capabilities
            .iter()
            .map(|c| c.to_string())
            .collect();
        // A pass: the live stack gets the candidate, and every screen is brought into line.
        self.lock().stack_mut().apply(cand).map_err(|why| {
            Refused::new(
                format!("`{name}` passed the gate but could not be applied"),
                why,
            )
        })?;
        let id = {
            let inner = self.lock();
            inner
                .stack
                .layers()
                .iter()
                .find(|l| l.dir == dir)
                .map(|l| l.id.clone())
                .unwrap_or_default()
        };
        self.refresh_panes(hub);
        let panes = pane_names(&self.lock().stack);
        let mine: Vec<String> = panes
            .into_iter()
            .filter(|p| p.starts_with(&format!("{name}/")))
            .collect();
        let pin_note = self.settle_pin(hub, name, &dir);
        Ok(format!(
            "loaded `{name}` as layer `{id}` — the shadow gate passed and it is answering now, with \
             no restart.\n\n{}\n\npanes it draws: {}\n\nit answers: {}\n\nit may: {}{}",
            said.join("\n"),
            if mine.is_empty() {
                "none (a layer that answers no pane hook)".to_string()
            } else {
                mine.join(", ")
            },
            if answers.is_empty() {
                "nothing".to_string()
            } else {
                format!("`{}`", answers.join("`, `"))
            },
            if wants.is_empty() {
                "nothing (no capability declared)".to_string()
            } else {
                wants.join(", ")
            },
            match &pin_note {
                Some(note) => format!("\n\n{note}"),
                None => String::new(),
            }
        ))
    }

    /// `plugin stack` - what is in force, newest first. The answering layer per hook is the topmost
    /// enabled one that exports it; whether it then passes or masks only a call answers.
    pub fn stack_doc(&self) -> String {
        let inner = self.lock();
        let stack = &inner.stack;
        let mut out = String::from(
            "the stack, newest first — session, then project, then user, then built-in.\n\n",
        );
        if stack.layers().is_empty() {
            out.push_str(
                "  no layers at all: nothing is patching anything. `plugin scaffold` writes the \
                 first one, and `plugin load` puts it in force.\n",
            );
        }
        for layer in stack.layers() {
            out.push_str(&format!(
                "  {:<10} {:<10} {:<26} {:<28} {:<12} {}{}\n",
                layer.id,
                layer.scope.to_string(),
                clip_chars_within(&layer.manifest.name, 26),
                clip_chars_within(&layer.manifest.hooks.join(","), 28),
                format!(
                    "[{}]",
                    layer
                        .manifest
                        .capabilities
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                if layer.sha256.is_empty() {
                    "sha —".to_string()
                } else {
                    format!("sha {}", &layer.sha256[..layer.sha256.len().min(12)])
                },
                if layer.disabled { "  DISABLED" } else { "" }
            ));
            // Where its source is: a pane that already exists is EXTENDED, and this is the path a
            // `read`/`edit` opens. A built-in has no directory, so it gets no line.
            if !layer.dir.as_os_str().is_empty() {
                out.push_str(&format!("  {:<10} at {}\n", "", layer.dir.display()));
            }
        }
        let mut hooks: Vec<String> = Vec::new();
        for layer in stack.layers() {
            for hook in &layer.manifest.hooks {
                if !hooks.contains(hook) {
                    hooks.push(hook.clone());
                }
            }
        }
        if !hooks.is_empty() {
            out.push_str(
                "\nhooks, and the topmost layer that exports each (whether it then passes or \
                 masks is what `plugin test` measures):\n",
            );
            for hook in hooks {
                let exporters: Vec<&letibot_plugins::Layer> = stack
                    .layers()
                    .iter()
                    .filter(|l| l.manifest.hooks.contains(&hook))
                    .collect();
                let answering = exporters.iter().find(|l| !l.disabled);
                let by = match answering {
                    Some(layer) => {
                        let also = exporters.len().saturating_sub(1);
                        format!(
                            "{} ({}){}",
                            layer.id,
                            layer.scope,
                            if also == 0 {
                                String::new()
                            } else {
                                format!(", {also} below")
                            }
                        )
                    }
                    None => format!(
                        "nobody — {} layer(s) export it and every one is disabled",
                        exporters.len()
                    ),
                };
                out.push_str(&format!(
                    "  {:<28} <- {:<38} {} recorded input(s)\n",
                    hook,
                    by,
                    inner.rec.len(&hook)
                ));
            }
        }
        let failures = stack.failures();
        out.push_str("\nfailures: ");
        if failures.is_empty() {
            // A declaration-only layer (a `plugin.toml`, no module - the documented install of a
            // built-in's tool) has no bytes and nothing failed, so say what `sha -` means.
            out.push_str(
                "none — nothing on disk failed to load. A layer with only a `plugin.toml` is a \
                 declaration, not a failure: the module that answers it is one the host already \
                 carries, which is why it is listed with `sha —` above.\n",
            );
        } else {
            out.push('\n');
            for f in failures {
                out.push_str(&format!("  {f}\n"));
            }
        }
        out.push_str(
            "\n`plugin test <name>` calls a hook and says which layer answered; `plugin disable \
             <layer>` takes one out without deleting it.\n",
        );
        out
    }

    /// `plugin disable` / `plugin enable` - the bisect for *which of my fixes broke this*. `what`
    /// is a layer id, a plugin's name or a pane id; a name matching two layers is refused with the
    /// ids rather than guessed at.
    pub fn set_enabled(
        &self,
        hub: &Arc<Hub>,
        what: &str,
        enabled: bool,
    ) -> Result<String, Refused> {
        let (id, name) = {
            let inner = self.lock();
            let layer = one_layer(&inner.stack, what)?;
            (layer.id.clone(), layer.manifest.name.clone())
        };
        {
            let mut inner = self.lock();
            inner
                .stack_mut()
                .set_disabled(&id, !enabled)
                .map_err(|why| Refused::new(format!("`{id}` could not be moved"), why))?;
        }
        self.refresh_panes(hub);
        let undo = if enabled {
            format!("disable {id}")
        } else {
            format!("enable {id}")
        };
        Ok(format!(
            "layer `{id}` (`{name}`) is {}. The stack re-resolved as if it were not there; nothing \
             was deleted, so `plugin {undo}` puts it back.",
            if enabled { "on" } else { "off" }
        ))
    }

    /// `plugin test` - one hook called with one input, and the whole answer reported. A pane id is
    /// asked through the whole stack, because that is a head's question; a layer id is asked with
    /// every other disk layer off, because that is a bisect's question.
    pub fn test(&self, what: Option<&str>, input: Option<&Value>) -> Result<String, Refused> {
        let stack = {
            let inner = self.lock();
            Arc::clone(&inner.stack)
        };
        // A layer that is BUILT and not loaded is still one `test` can ask, on a copy: the loop
        // tests before anything is applied, and "not a layer of this session" is no answer to a
        // question about a module that is there on disk.
        let (stack, unloaded) = self.asking_stack(&stack, what)?;
        let (hook, only, label) = self.resolve_hook(&stack, what)?;
        // The synthetic input is of the hook's kind, not a pane's: a `condition` handed a pane's
        // first frame answers nothing and reads as a broken layer.
        let input = input
            .cloned()
            .unwrap_or_else(|| synthetic_for(&hook, &self.feeds_for(&hook)));
        let rec = self.lock().rec.clone();
        let asked = match &only {
            None => stack.clone(),
            Some(id) => {
                // Every other disk layer off: a built-in is the floor and stays, because a layer
                // that asks below would otherwise be told the stack is empty.
                let mut alone = stack.clone();
                for layer in stack.layers() {
                    if layer.scope != Scope::BuiltIn && &layer.id != id {
                        let _ = alone.mask(&layer.id, true);
                    }
                }
                alone
            }
        };
        let resolved = asked.call(&hook, &input);
        let mut out = format!("`{hook}` asked with {} — {label}\n", describe_input(&input));
        if let Some(dir) = &unloaded {
            out.push_str(&format!(
                "\n`{}` is built but NOT loaded: it ran alone on a copy of this session's stack, \
                 so nothing on any screen changed. `plugin load` is what puts it in force.\n",
                dir.display()
            ));
        }
        for f in &resolved.failures {
            out.push_str(&format!("\nTRAP: {f}\n"));
        }
        let recorded = rec.len(&hook);
        match &resolved.answer {
            None => {
                out.push_str(
                    "\nNothing answered: every layer that exports this hook passed (or masked it), \
                     and there is nothing below.\n",
                );
            }
            Some(answer) => {
                let by = resolved.by.clone().unwrap_or_default();
                let fuel = asked.fuel(&by);
                let scope = asked
                    .layer(&by)
                    .map(|l| l.scope.to_string())
                    .unwrap_or_else(|| "?".to_string());
                // The acts the answering layer declared, so the diagnostic asks what the running
                // host asks: a row reaching a verb the manifest never named is refused here too.
                let declared = asked
                    .layer(&by)
                    .map(|l| l.manifest.actions.clone())
                    .unwrap_or_default();
                out.push_str(&format!(
                    "\nanswered by `{by}` ({scope}){}\n",
                    match fuel {
                        Some(n) => format!(" · {n} fuel units"),
                        None => String::new(),
                    }
                ));
                // The source it answered from, so the next step is an `edit` and not a scaffold.
                if let Some(dir) = asked
                    .layer(&by)
                    .map(|l| &l.dir)
                    .filter(|d| !d.as_os_str().is_empty())
                {
                    out.push_str(&format!("answered from {}\n", dir.display()));
                }
                match accepted(&hook, answer, &declared, &input) {
                    Ok(what) => {
                        out.push_str(&format!("\nit answers — {what}:\n"));
                        out.push_str(
                            &serde_json::to_string_pretty(answer)
                                .unwrap_or_else(|_| answer.to_string()),
                        );
                        out.push('\n');
                        // A pane that asks for a look is the frame that asks; say so, because an
                        // answer with no numbers in it otherwise reads as a broken dashboard.
                        if hook.starts_with("pane_")
                            && let Ok(view) = validated(answer, &hook, &by, &declared)
                            && let Some(spec) = &view.look
                        {
                            out.push_str(&format!(
                                "\nit asks the host for a look: {} — a head runs it and \
                                         calls the pane again with the result, so this frame is \
                                         the one that asks.\n",
                                serde_json::to_string(spec).unwrap_or_default()
                            ));
                        }
                    }
                    Err(why) => {
                        out.push_str(&format!(
                            "\n**The host would REFUSE this answer**, so the session would go on \
                             without it (a pane draws the refusal instead): {why}\n\nthe answer \
                             was:\n{}\n",
                            serde_json::to_string_pretty(answer)
                                .unwrap_or_else(|_| answer.to_string())
                        ));
                    }
                }
            }
        }
        out.push_str(&format!(
            "\n{recorded} input(s) of this hook recorded for the shadow gate's replay.\n"
        ));
        Ok(out)
    }

    /// The stack `test` asks, and the source it had to build to ask it. A name the live stack does
    /// not hold is looked for in this session's plugin directory: a built module there is a layer
    /// the loop has not loaded yet, and asking it on a copy is what makes `test` a step that comes
    /// before `load` rather than after it. A directory with no module is refused for `plugin build`.
    fn asking_stack(
        &self,
        stack: &Stack,
        what: Option<&str>,
    ) -> Result<(Stack, Option<PathBuf>), Refused> {
        let Some(name) = what.map(str::trim).filter(|w| !w.is_empty()) else {
            return Ok((stack.clone(), None));
        };
        // A pane id names its plugin before the slash.
        let plugin = name.split('/').next().unwrap_or(name);
        let loaded = stack
            .layers()
            .iter()
            .any(|l| l.id == name || l.manifest.name == plugin);
        if loaded {
            return Ok((stack.clone(), None));
        }
        // Not in this session: `layer_dir` names what is, and the stack's own refusal is better
        // than a second guess about which plugin was meant.
        let Ok(dir) = self.layer_dir(plugin) else {
            return Ok((stack.clone(), None));
        };
        let cand = stack.candidate(&dir, Scope::Session).map_err(|why| {
            Refused::new(
                format!("`{plugin}` is not built yet, so there is nothing to ask"),
                format!("{why} — `plugin build {plugin}` produces the module a layer loads"),
            )
        })?;
        let mut shadow = stack.clone();
        shadow.apply(cand).map_err(|why| {
            Refused::new(
                format!("`{plugin}` is built but cannot be asked"),
                format!("{why} — nothing was applied; `plugin load {plugin}` is the door that applies it"),
            )
        })?;
        Ok((shadow, Some(dir)))
    }

    /// Which hook `test` asks, and whether it is asked alone: a pane id is the whole stack's
    /// question, a layer id is that layer's own.
    fn resolve_hook(
        &self,
        stack: &Stack,
        what: Option<&str>,
    ) -> Result<(String, Option<String>, String), Refused> {
        let panes = pane_names(stack);
        match what.map(str::trim).filter(|w| !w.is_empty()) {
            // A pane id is `<plugin>/<hook name without its kind>` (C4).
            Some(pane) if pane.contains('/') => {
                let (_, name) = pane.split_once('/').unwrap_or((pane, ""));
                if name.is_empty() {
                    return Err(Refused::new(
                        format!("`{pane}` is not a pane id"),
                        pane_list(&panes),
                    ));
                }
                Ok((
                    format!("pane_{name}"),
                    None,
                    format!("through the whole stack, as a head asks it (`{pane}`)"),
                ))
            }
            Some(other) => {
                let layer = one_layer(stack, other)?;
                let hook = layer
                    .manifest
                    .hooks
                    .iter()
                    .find(|h| letibot_plugins::is_hook_kind(h))
                    .cloned()
                    .ok_or_else(|| {
                        Refused::new(
                            format!("layer `{}` answers no hook this host runs", layer.id),
                            format!(
                                "its plugin.toml declares {:?}; the hook kinds are the prefixes \
                                 {} (`on_tool_call` and `on_tool_result` are the `on_` ones)",
                                layer.manifest.hooks,
                                letibot_plugins::SERVED_KINDS.join(", ")
                            ),
                        )
                    })?;
                Ok((
                    hook,
                    Some(layer.id.clone()),
                    format!(
                        "with every other layer of this session switched off (layer `{}` alone)",
                        layer.id
                    ),
                ))
            }
            None => match panes.len() {
                1 => {
                    let (_, name) = panes[0].split_once('/').unwrap_or(("", ""));
                    Ok((
                        format!("pane_{name}"),
                        None,
                        format!("through the whole stack (`{}` is the only pane)", panes[0]),
                    ))
                }
                _ => {
                    let all: Vec<String> = stack
                        .layers()
                        .iter()
                        .flat_map(|l| l.manifest.hooks.clone())
                        .collect();
                    Err(Refused::new(
                        if all.is_empty() {
                            "this session has no hook to test".to_string()
                        } else {
                            "name the hook to test".to_string()
                        },
                        if all.is_empty() {
                            "`plugin scaffold` writes one, `plugin build` compiles it, and \
                             `plugin load` puts it in force; or name a layer by id"
                                .to_string()
                        } else {
                            format!("the hooks of this session are: {}", all.join(", "))
                        },
                    ))
                }
            },
        }
    }
}

/// The first frame of any hook, for `plugin test` and for `load`'s replay of a layer nobody has
/// asked yet: the SDK's own synthetic example of that kind. A layer that cannot answer it traps on
/// its own first frame, which is the cheapest defect to find.
fn synthetic_for(hook: &str, feeds: &BTreeMap<String, Value>) -> Value {
    use letibot_plugin::{
        card, compact, condition, digest, link, nag, notice, push, steer, tool_call, tool_result,
    };
    let value = if hook.starts_with("pane_") {
        return synthetic_pane_input(feeds);
    } else if hook == tool_call::HOOK {
        serde_json::to_value(tool_call::ToolCallIn::synthetic())
    } else if hook == tool_result::HOOK {
        serde_json::to_value(tool_result::ToolResultIn::synthetic())
    } else if hook.starts_with(condition::PREFIX) {
        serde_json::to_value(condition::ConditionIn::synthetic())
    } else if hook.starts_with(digest::PREFIX) {
        serde_json::to_value(digest::Input::synthetic())
    } else if hook.starts_with(link::PREFIX) {
        serde_json::to_value(link::Input::synthetic())
    } else if hook.starts_with(nag::PREFIX) {
        serde_json::to_value(nag::Input::synthetic())
    } else if hook.starts_with(card::PREFIX) {
        serde_json::to_value(card::Input::synthetic())
    } else if hook == steer::HOOK {
        serde_json::to_value(steer::Input::synthetic())
    } else if hook == compact::HOOK {
        serde_json::to_value(compact::Input::synthetic())
    } else if hook.starts_with(push::PREFIX) {
        serde_json::to_value(push::Input::synthetic())
    } else {
        serde_json::to_value(notice::Notice::synthetic())
    };
    value.unwrap_or(Value::Null)
}

/// Would the host accept this answer for `hook`? - the same parse the running host does, in one
/// place, so `plugin test` and `load` cannot disagree with what a session would live with. `Ok` is
/// the one line a person reads (what the answer would do); `Err` is the refusal.
fn accepted(
    hook: &str,
    answer: &Value,
    declared: &[String],
    input: &Value,
) -> Result<String, String> {
    use letibot_plugin::{
        card, compact, condition, digest, link, nag, notice, push, steer, tool_call, tool_result,
    };
    if hook.starts_with("pane_") {
        let view = validated(answer, hook, hook, declared)?;
        return Ok(format!("a view the host can drive — \"{}\"", view.title));
    }
    if hook == tool_call::HOOK {
        let out: tool_call::ToolCallOut = serde_json::from_value(answer.clone())
            .map_err(|e| format!("not a call's verdict ({e})"))?;
        return Ok(match out.verdict() {
            tool_call::Verdict::Pass => "it lets the call run as the model proposed it".into(),
            tool_call::Verdict::Rewrite(args) => {
                format!("it runs the call with these arguments instead: {args}")
            }
            tool_call::Verdict::Refuse(why) => format!("it refuses the call: {why}"),
            tool_call::Verdict::Answer(text) => {
                format!(
                    "it answers the call without running it: {text} — the gate never sees that \
                     call, so this host takes the answer from a pinned layer or a built-in only"
                )
            }
        });
    }
    if hook == tool_result::HOOK {
        let out: tool_result::ToolResultOut = serde_json::from_value(answer.clone())
            .map_err(|e| format!("not a tool result ({e})"))?;
        let mut said = Vec::new();
        if out.payload.is_some() {
            said.push("it replaces the result".to_string());
        }
        if let Some(append) = &out.append {
            said.push(format!("it appends {append:?}"));
        }
        for arm in &out.arm {
            said.push(format!("it arms a `{}` watch on {}", arm.plugin, arm.args));
        }
        return Ok(if said.is_empty() {
            "it leaves the result alone".to_string()
        } else {
            said.join("; ")
        });
    }
    if hook.starts_with(digest::PREFIX) {
        let out: digest::Out =
            serde_json::from_value(answer.clone()).map_err(|e| format!("not a digest ({e})"))?;
        // Checked against the input the hook was actually asked with: a digest is a statement about
        // the bytes it was handed, and the synthetic example is that only on its own first frame.
        let was = input["payload"].as_str().unwrap_or_default();
        return Ok(match out.checked(was)? {
            None => "it leaves the output as the tool produced it".to_string(),
            Some(text) => format!(
                "it hands on {} bytes of the {} the call produced: {text:?}",
                text.len(),
                was.len()
            ),
        });
    }
    if hook.starts_with(link::PREFIX) {
        let out: link::Out = serde_json::from_value(answer.clone())
            .map_err(|e| format!("not a link answer ({e})"))?;
        let words: Vec<String> = input["words"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let urls = out.checked(&words)?;
        return Ok(match out.linked() {
            0 => "it links none of the words a frame would ask about".to_string(),
            n => format!(
                "it links {n} of {} word(s): {}",
                words.len(),
                words
                    .iter()
                    .zip(&urls)
                    .filter_map(|(w, u)| u.as_ref().map(|u| format!("`{w}` → {u}")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    if hook.starts_with(condition::PREFIX) {
        let out: condition::ConditionOut =
            serde_json::from_value(answer.clone()).map_err(|e| format!("not a condition ({e})"))?;
        let mut said = Vec::new();
        if let Some(look) = &out.look {
            said.push(format!("it looks at {look:?}"));
        }
        if let Some(every) = out.every_s {
            said.push(format!("every {every}s"));
        }
        if let Some(fired) = &out.fired {
            said.push(format!("and it fires: {fired}"));
        }
        return Ok(if said.is_empty() {
            "it watches nothing yet".to_string()
        } else {
            said.join(", ")
        });
    }
    if hook.starts_with(card::PREFIX) {
        let out = card::read(answer)?;
        let asked: card::Input =
            serde_json::from_value(input.clone()).map_err(|e| format!("not a card input ({e})"))?;
        return Ok(out.said(asked.parts.len()));
    }
    if hook.starts_with(nag::PREFIX) {
        let out = nag::read(answer)?;
        let input: nag::Input =
            serde_json::from_value(input.clone()).map_err(|e| format!("not a nag input ({e})"))?;
        return Ok(out.said(&input));
    }
    if hook == steer::HOOK {
        let out = steer::Answer::parse(answer.clone())?;
        return Ok(match out {
            steer::Answer::Continue(text) => format!("it carries the turn on: {text}"),
            steer::Answer::Stop => "it ends the turn".to_string(),
        });
    }
    if hook == compact::HOOK {
        let out = compact::Answer::parse(answer.clone())?;
        let mut said = Vec::new();
        if !out.sections.is_empty() {
            said.push(format!("the record keeps {}", out.sections.join(", ")));
        }
        if let Some(extra) = &out.extra_instruction {
            said.push(format!("it adds: {extra}"));
        }
        if !out.pin.is_empty() {
            said.push(format!("it pins {}", out.pin.join(", ")));
        }
        said.push(match out.after {
            compact::After::Continue => "and the session goes on after it".to_string(),
            compact::After::Wait => "and the session waits after it".to_string(),
        });
        return Ok(said.join(", "));
    }
    if hook.starts_with(push::PREFIX) {
        let out = push::read(answer)?;
        let input: push::Input =
            serde_json::from_value(input.clone()).map_err(|e| format!("not a push input ({e})"))?;
        return Ok(out.said(&input));
    }
    let out: notice::NoticeOut =
        serde_json::from_value(answer.clone()).map_err(|e| format!("not a notice ({e})"))?;
    Ok(match out.said() {
        notice::Said::Unchanged => "it leaves the harness's own sentence".to_string(),
        notice::Said::Text(text) => format!("it would say: {text}"),
        notice::Said::Drop => "it asks for the notice to be dropped".to_string(),
    })
}

/// One layer, by id, by manifest name, or by pane id - and never a guess between two. A name that
/// matches two layers is refused with the ids: *disable the plugin called x* has two answers
/// there, and picking one would move a layer nobody named.
fn one_layer<'a>(stack: &'a Stack, what: &str) -> Result<&'a letibot_plugins::Layer, Refused> {
    let what = what.trim();
    if let Some(layer) = stack.layer(what) {
        return Ok(layer);
    }
    // A pane id names its plugin before the slash.
    let by_name = what.split('/').next().unwrap_or(what);
    let matches: Vec<&letibot_plugins::Layer> = stack
        .layers()
        .iter()
        .filter(|l| l.manifest.name == by_name)
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => {
            let ids: Vec<String> = stack
                .layers()
                .iter()
                .map(|l| format!("{} ({})", l.id, l.manifest.name))
                .collect();
            Err(Refused::new(
                format!("`{what}` is not a layer of this session"),
                if ids.is_empty() {
                    "the stack is empty: nothing is patching anything yet".to_string()
                } else {
                    format!("the layers are: {}", ids.join(", "))
                },
            ))
        }
        _ => Err(Refused::new(
            format!("`{what}` names {} layers", matches.len()),
            format!(
                "there is more than one layer called `{by_name}`; name it by id: {}",
                matches
                    .iter()
                    .map(|l| format!("{} ({})", l.id, l.scope))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// How an input reads in a sentence, whatever kind it is: a pane's frame, a notice, a call, a
/// result, or a watch - the fields the reader needs to recognise which input went wrong.
fn describe_any_input(input: &Value) -> String {
    if input.get("size").is_some() || input.get("event").is_some() {
        return describe_input(input);
    }
    if let Some(kind) = input.get("kind").and_then(|k| k.as_str()) {
        return format!("a `{kind}` notice");
    }
    if let Some(call) = input.get("call") {
        let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        return match input.get("outcome").and_then(|o| o.as_str()) {
            Some(outcome) => format!("the result of `{name}` ({outcome})"),
            None => format!("a `{name}` call"),
        };
    }
    if input.get("args").is_some() {
        return match input.get("look") {
            Some(look) if !look.is_null() => "a look's result".to_string(),
            _ => "the arming call".to_string(),
        };
    }
    "this input".to_string()
}

/// How an input reads in a sentence: which frame it is, said the way a person would say it.
fn describe_input(input: &Value) -> String {
    let mut parts = Vec::new();
    if input["look"].is_null() {
        parts.push("no look".to_string());
    } else {
        parts.push(format!("a look's result (exit {})", input["look"]["exit"]));
    }
    if !input["event"].is_null() {
        parts.push(format!(
            "the `{}` action on `{}`",
            input["event"]["action"].as_str().unwrap_or("?"),
            input["event"]["item"].as_str().unwrap_or("?")
        ));
    }
    if !input["state"].is_null() {
        parts.push("the state it returned last time".to_string());
    }
    format!("[{}]", parts.join(", "))
}

fn fuel_note(fuel: u64) -> String {
    if fuel == 0 {
        // Nothing measured is not *it cost nothing*: a module that has not been asked burns no
        // fuel, and saying `0` would report a free hook that never ran.
        String::new()
    } else {
        format!(", {fuel} fuel units")
    }
}

fn pane_list(panes: &[String]) -> String {
    if panes.is_empty() {
        "this session has no panes: nothing is patching anything yet".to_string()
    } else {
        format!("the panes of this session are: {}", panes.join(", "))
    }
}
