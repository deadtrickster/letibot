//! The plugin layers a session has, and the panes they answer: the daemon's half of the
//! plugin stack - which session holds which `Stack`, when a look runs, and what reaches a
//! head. The answer protocol, the precedence and the gate belong to `letibot_plugins`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use letibot_plugins::{
    Builder, Capability, CargoBuilder, DeclaredTool, Failure, Loader, Manifest, Recorder, Scope,
    Stack,
};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::pane::{Block, Line, LookSpec, Panel, Role, Unavailable, View};
use letibot_sessionlog::registry::PaneDriver;
use letibot_transcript::text::clip_chars_within;
use serde_json::{Value, json};

pub mod conditions;
pub mod contract;
pub mod desk;
pub mod feed;
pub mod look;
pub mod rano;
pub mod room_feed;
pub mod scaffold;
// A tool a layer declares, and the looks its hook asks the host to run: it is a `Tool` in the
// registry beside `read` and `bash`, and its three look primitives are a filesystem API.
pub mod tool;

#[cfg(test)]
mod tests;

/// How often the hot-reload tick looks: the longest a person waits after saving a layer. Two
/// seconds, as `mergequeue::watch` polls - a stat per layer is free.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// The rectangle a pane draws in when the head did not say one: a head too old to send its own
/// `w` and `h` gets a wide screen rather than a guess about its terminal.
pub const SIZE_W: u32 = 120;
pub const SIZE_H: u32 = 40;

/// The contract version every pane input carries. It is the SDK's constant and not a copy: a
/// hardcoded `1` outlived the contract's growth to 2, so a layer that read `v` was told no feed
/// by the host about to hand it one.
const PANE_V: u32 = letibot_plugin::pane::V;

/// One session's layers, its recorded inputs, and the panes a head has opened.
pub struct PluginHost {
    session: String,
    workspace: PathBuf,
    /// The session's checkout: the repository its tree belongs to (a worktree's is the checkout it
    /// was cut from), where a pane's relative `read` is looked for first - see [`look::Trees`].
    checkout: PathBuf,
    /// The session's own plugin directory - where `plugin scaffold` writes and where a
    /// session-scope layer is discovered. Beside the stack, which only knows what it found.
    session_dir: Option<PathBuf>,
    inner: Mutex<Inner>,
    /// How the gate's build step is performed: `CargoBuilder` in the daemon, a table of answers
    /// in a test, which cannot pay for the wasm target and a crate to build.
    builder: Arc<dyn Builder>,
    /// Where a pane's `sessions` feed comes from - the daemon's registry, set by
    /// [`PluginHost::with_sessions`]. `None` in a host a test built, and a layer that declares
    /// the feed is then handed no key at all rather than an empty list (see [`feed`]).
    sessions: Option<Arc<dyn feed::SessionFeed>>,
    /// Where a pane's view of the room comes from - the session's own connection to the box's
    /// coordinator, set by [`PluginHost::with_room`]. `None` in a test's host and on a box with no
    /// coordinator, and the `room` feed is then no key at all rather than an empty room.
    room: Mutex<Option<Arc<dyn room_feed::RoomFeed>>>,
    /// Whether this host installs the binary's own pane layers at the door a head comes through.
    /// The daemon's host does ([`PluginHost::production`]); a host built on a test's loader cannot,
    /// because a fake loader has no engine for a module held in memory.
    install_builtin_panes: bool,
    /// A head has arrived and the built-in panes are wanted; the next [`PluginHost::tick`] compiles
    /// them on a thread, so the attach itself never waits seconds for a compiler.
    builtins_wanted: std::sync::atomic::AtomicBool,
    /// The compile has been started once; it is not started twice.
    builtins_started: std::sync::atomic::AtomicBool,
}

struct Inner {
    stack: Stack,
    rec: Recorder,
    /// The panes a head has opened, by id, with what the pane returned and when it wants asking
    /// again.
    panes: BTreeMap<String, Open>,
    /// The rectangle each pane was last asked to draw in. Beside `panes` rather than inside an
    /// `Open` because a close is not a forget: reopened on the same screen, it draws at that size.
    sizes: BTreeMap<String, (u32, u32)>,
    /// Layer ids with a gate running right now, so a poll that sees the same change twice does
    /// not compile it twice.
    gating: BTreeSet<String>,
    /// What has already been said on the session's log. A failure said once is news; the same
    /// failure said every two seconds is noise a model learns to ignore.
    said: BTreeSet<String>,
    /// What the session's model has not been told yet. Drained by the worker, because a notice is
    /// a transcript row and only a harness writes one of those.
    notices: Vec<String>,
    /// When the next hot-reload poll is due.
    poll_at: Instant,
    /// Pins whose bytes moved before this host existed, held for the first tick that can say them.
    broken_pins: Vec<letibot_plugins::BrokenPin>,
}

/// What the host knows about one open pane.
struct Open {
    /// The `state` the pane answered with, handed back to it on the next call
    /// state belongs to the host, and a plugin is stateless between calls.
    state: Option<Value>,
    /// The look the pane's last answer asked for, so a refresh can run it again.
    spec: Option<LookSpec>,
    /// The layer that answered, whose capabilities a look is checked against.
    by: String,
    /// 0 is never; C3's floor is `sessionlog::pane::MIN_EVERY_S`.
    every_s: u64,
    next_at: Option<Instant>,
}

/// What one lifecycle event came to, for the caller that performs it: the acts a layer asked for,
/// already checked against its manifest's `actions`. There is no field for stopping the event,
/// because the answer a layer writes has none - that is what *a layer cannot block a reseat* means.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lifecycle {
    /// The layer that answered, when one did. Here so a caller can say whose acts these are.
    pub by: Option<String>,
    /// One act per entry: the verb `plugin.toml` declares, and its arguments.
    pub acts: Vec<(String, Value)>,
}

/// What one render is: which `look` and `event` the first call is handed, and whether this pass
/// may run the look the answer asks for.
struct Req {
    look: Option<Value>,
    event: Option<(String, String)>,
    may_look: bool,
}

impl Req {
    /// The pane's first frame: nothing looked at, nothing pressed.
    fn fresh() -> Req {
        Req {
            look: None,
            event: None,
            may_look: true,
        }
    }

    /// The same, for a caller that must not run a look: announcing a pane to a head that has not
    /// opened it.
    fn announced() -> Req {
        Req {
            may_look: false,
            ..Req::fresh()
        }
    }
}

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
        let mut stack = Stack::discover(
            session_dir.as_deref(),
            Some(&workspace),
            user_dir.as_deref(),
            loader,
        );
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
                stack,
                rec: Recorder::new(),
                panes: BTreeMap::new(),
                sizes: BTreeMap::new(),
                gating: BTreeSet::new(),
                said: BTreeSet::new(),
                notices: Vec::new(),
                poll_at: Instant::now() + POLL_EVERY,
                broken_pins: Vec::new(),
            }),
            builder,
            sessions: None,
            room: Mutex::new(None),
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
        let news = self.lock().stack.set_trust(trust);
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

    /// Whether the layer answering `hook` runs freely: a built-in, or a layer whose bytes a person
    /// pinned. An `exec` look asks this before the gate.
    pub fn free_layer(&self, hook: &str) -> bool {
        self.answering(hook).is_some_and(|a| a.trusted)
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
        host
    }

    /// Put the built-in layers a session declared a tool from at the bottom of this stack, matched
    /// on the tool's name. A project's ask can then be a manifest with no module: the built-in is
    /// the module that answers.
    fn install_builtin_tools(&self, declared: &[DeclaredTool]) {
        if declared.is_empty() {
            return;
        }
        let mut inner = self.lock();
        for builtin in letibot_builtins::table() {
            let declares = Manifest::parse(builtin.manifest)
                .map(|m| {
                    m.tools
                        .iter()
                        .any(|t| declared.iter().any(|d| d.tool.name == t.name))
                })
                .unwrap_or(false);
            if !declares {
                continue;
            }
            if let Err(why) =
                inner
                    .stack
                    .register_builtin_bytes(builtin.name, builtin.manifest, builtin.wasm)
            {
                // The stack has recorded the failure; this is the line the daemon's log shows.
                eprintln!("  plugin built-in `{}` did not load: {why}", builtin.name);
            }
        }
    }

    fn room_source(&self) -> Option<Arc<dyn room_feed::RoomFeed>> {
        self.room.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Give a host built before the session had a room its connection, and show the pane to a head
    /// that is already attached.
    pub fn link_room(&self, source: Arc<dyn room_feed::RoomFeed>, hub: &Arc<Hub>) {
        *self.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
        PaneDriver::attach(self, "", hub);
    }

    /// Install the binary's built-in pane layers the first time a head comes through a door that
    /// needs them - a module costs seconds to compile, and a session nobody attached to must not pay
    /// for one. Every built-in pane is then listed by `/pane`, tool declared or not.
    /// Whether a head asked for the built-in panes and their compile has not started yet.
    fn builtins_due(&self) -> bool {
        self.install_builtin_panes
            && self
                .builtins_wanted
                .load(std::sync::atomic::Ordering::SeqCst)
            && !self
                .builtins_started
                .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn ensure_builtin_panes(&self) {
        if !self.install_builtin_panes {
            return;
        }
        for builtin in letibot_builtins::table() {
            let Ok(manifest) = Manifest::parse(builtin.manifest) else {
                continue;
            };
            let panes: Vec<&String> = manifest
                .hooks
                .iter()
                .filter(|h| h.strip_prefix("pane_").is_some_and(|n| !n.is_empty()))
                .collect();
            if panes.is_empty() {
                continue;
            }
            // A layer that already answers the hook is the one that draws it - ours would sit below
            // it and never be called, and a project that shipped its own pane has said which it
            // wants. The layer itself, installed for a declared tool, is that layer too.
            let answered = {
                let inner = self.lock();
                inner
                    .stack
                    .layers()
                    .iter()
                    .any(|l| !l.disabled && l.manifest.hooks.iter().any(|h| panes.contains(&h)))
            };
            if answered {
                continue;
            }
            if let Err(why) = self.lock().stack.register_builtin_bytes(
                builtin.name,
                builtin.manifest,
                builtin.wasm,
            ) {
                eprintln!("  plugin built-in `{}` did not load: {why}", builtin.name);
            }
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    /// The trees a look reads in: the session's checkout first, then the tree it stands in.
    pub fn look_trees(&self) -> look::Trees<'_> {
        look::Trees::new(&self.checkout, &self.workspace)
    }

    /// Ask one hook of this session's stack, recording the input for the gate's replay. `None` when
    /// nothing here exports the hook, which keeps a session that patches nothing as cheap as it was.
    /// A layer that traps is skipped, the layer below answers, and the model is told.
    pub fn ask(
        &self,
        hub: &Arc<Hub>,
        hook: &str,
        input: &Value,
    ) -> Option<letibot_plugins::Resolved> {
        let mut inner = self.lock();
        let answering = inner.stack.layers().iter().any(|l| {
            !l.disabled
                && l.manifest.contract == letibot_plugins::CONTRACT
                && l.manifest.hooks.iter().any(|h| h == hook)
        });
        if !answering {
            return None;
        }
        inner.rec.record(hook, input);
        let resolved = inner.stack.call(hook, input);
        say_failures(hub, &mut inner, &resolved.failures);
        Some(resolved)
    }

    /// One lifecycle event through this session's stack (X21): the acts a layer asked for, checked
    /// against its manifest's `actions`, for the caller to perform. Nothing here can stop the event -
    /// there is no field for it in a [`letibot_plugin::lifecycle::Answer`] - and the event has already
    /// happened by the time anybody asks.
    pub fn lifecycle(&self, hub: &Arc<Hub>, event: &str, input: &Value) -> Lifecycle {
        let mut out = Lifecycle::default();
        let Some(resolved) = self.ask(hub, &letibot_plugin::lifecycle::hook(event), input) else {
            return out;
        };
        out.by = resolved.by.clone();
        let Some(answer) = resolved.answer else {
            return out;
        };
        let by = resolved.by.unwrap_or_default();
        let declared = self
            .layer(&by)
            .map(|l| l.manifest.actions)
            .unwrap_or_default();
        let parsed = match letibot_plugin::lifecycle::Answer::parse(answer.clone()) {
            Ok(parsed) => parsed,
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "{why}, and a lifecycle answer is `{{\"acts\": [{{\"verb\": …, \"args\": …}}]}}` \
                         and nothing else: {answer}"
                    ),
                );
                return out;
            }
        };
        // All or nothing: an answer naming an act the manifest did not declare is the layer's bug,
        // and it loses the whole answer rather than half of it - the same rule a pane view has.
        if let Err(why) = parsed.validate_for(&declared) {
            self.complain(hub, &by, &why);
            return out;
        }
        out.acts = parsed
            .acts
            .into_iter()
            .map(|a| (a.verb, a.args.unwrap_or(Value::Null)))
            .collect();
        out
    }

    /// One notice through the `notice` hook: the sentence to record, or `None` when a layer dropped
    /// it. Which kinds may be dropped is `letibot_plugin::notice`'s answer, not this function's, and
    /// a layer that drops one the model is owed is refused and told why.
    pub fn notice(&self, hub: &Arc<Hub>, kind: &str, text: &str, facts: Value) -> Option<String> {
        let input = letibot_plugin::notice::Notice::new(kind, text, facts);
        let input = match serde_json::to_value(&input) {
            Ok(input) => input,
            // A notice that cannot be serialised is a bug in the host, not a layer's, and a
            // sentence the model is owed is never lost to it.
            Err(_) => return Some(text.to_string()),
        };
        let Some(resolved) = self.ask(hub, &letibot_plugin::notice::hook(kind), &input) else {
            // Nothing in this session answers this kind: the harness's own sentence, unchanged.
            return Some(text.to_string());
        };
        let Some(answer) = resolved.answer else {
            return Some(text.to_string());
        };
        let by = resolved.by.clone().unwrap_or_default();
        let out: letibot_plugin::notice::NoticeOut = match serde_json::from_value(answer.clone()) {
            Ok(out) => out,
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a notice, which is \
                         `{{\"text\": \"…\"}}` or `{{\"drop\": true}}` and nothing else: \
                         {why}. Its answer was {answer}, so the harness's own sentence stands"
                    ),
                );
                return Some(text.to_string());
            }
        };
        match out.said() {
            letibot_plugin::notice::Said::Unchanged => Some(text.to_string()),
            letibot_plugin::notice::Said::Text(said) => Some(said.to_string()),
            letibot_plugin::notice::Said::Drop => {
                if letibot_plugin::notice::droppable(kind) {
                    return None;
                }
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` asked for the `{kind}` notice to be dropped, and a `{kind}` \
                         notice may be reworded but not dropped — it is a fact the model is owed \
                         (docs/plugins-plan.md §4.3). The harness's own sentence stands"
                    ),
                );
                Some(text.to_string())
            }
        }
    }

    /// A layer's answer the host could not use, said once - to the model, which can fix the layer
    /// and to the session's log, so a person watching sees why nothing changed. Not through the
    /// `notice` hook: a layer that fails must not fail again about its own failure.
    fn complain(&self, hub: &Arc<Hub>, layer: &str, what: &str) {
        let said = format!("a plugin layer's answer was refused: {what}");
        {
            let mut inner = self.lock();
            if !inner.said.insert(said.clone()) {
                return;
            }
            inner.notices.push(said.clone());
        }
        hub.warn(
            "plugin_layer",
            if layer.is_empty() {
                said
            } else {
                format!("{layer}: {said}")
            },
        );
    }

    /// The pane ids this session's layers export: `<manifest name>/<hook name>`, read from the
    /// manifest rather than the module, whose exports `Manifest::admit` has already made equal. A
    /// layer whose module failed to load is listed too: a pane that answers nothing says so.
    pub fn panes(&self) -> Vec<String> {
        pane_names(&self.lock().stack)
    }

    /// Say what discovery could not load, and publish every pane's first frame, so a head knows a
    /// session's panes before anybody opens one. No look runs here: a dashboard nobody opened must
    /// not read files on the session's behalf.
    pub fn announce(&self, hub: &Arc<Hub>) {
        self.say_failures_now(hub);
        for pane in self.panes() {
            self.render(hub, &pane, Req::announced());
        }
    }

    /// What this session's model has not been told yet, handed over and forgotten.
    pub fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().notices)
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
            broken.extend(inner.stack.refresh_trust());
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
                !inner.stack.poll_changes().is_empty()
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
            inner.stack.poll_changes()
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
                (inner.stack.clone(), inner.rec.clone())
            };
            let report = letibot_plugins::gate(&stack, &rec, &cand, self.builder.as_ref());
            let refusal = if report.passed {
                let mut inner = self.lock();
                inner.stack.apply(cand).err()
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
                let news = self.lock().stack.refresh_trust();
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

    /// The stack changed: bring every screen into line with it. A pane still there is asked again
    /// with its look (a reload is a new version of the answer, not a reason to redraw the frame that
    /// asks for the file), a gone one is published as `PluginPaneGone`, and a new one is announced.
    fn refresh_panes(&self, hub: &Arc<Hub>) {
        let panes = self.panes();
        let (stale, open): (Vec<String>, Vec<String>) = {
            let inner = self.lock();
            inner
                .panes
                .keys()
                .cloned()
                .partition(|p| !panes.contains(p))
        };
        {
            let mut inner = self.lock();
            for pane in &stale {
                inner.panes.remove(pane);
            }
        }
        for pane in &stale {
            hub.publish(SessionEvent::PluginPaneGone { pane: pane.clone() });
        }
        let fresh: Vec<String> = panes
            .iter()
            .filter(|p| !open.contains(p))
            .cloned()
            .collect();
        for pane in open {
            self.render(hub, &pane, Req::fresh());
        }
        for pane in fresh {
            self.render(hub, &pane, Req::announced());
        }
    }

    /// Every open pane whose `every_s` has come round, asked again.
    fn refresh_due(&self, hub: &Arc<Hub>) {
        let now = Instant::now();
        let due: Vec<String> = {
            let inner = self.lock();
            inner
                .panes
                .iter()
                .filter(|(_, p)| p.next_at.is_some_and(|at| at <= now))
                .map(|(name, _)| name.clone())
                .collect()
        };
        for pane in due {
            // The look comes first, because that is what a refresh is: the pane asked for the
            // file, not for a call - a second call with no new look would draw the same numbers.
            let (spec, by) = {
                let inner = self.lock();
                match inner.panes.get(&pane) {
                    Some(open) => (open.spec.clone(), open.by.clone()),
                    None => continue,
                }
            };
            let look = spec.map(|spec| {
                let caps = self.caps_of(&by);
                look::run(&spec, self.look_trees(), &caps)
            });
            self.render(
                hub,
                &pane,
                Req {
                    look,
                    event: None,
                    may_look: true,
                },
            );
        }
    }

    /// The capabilities the layer that answered declared - what a look is checked against. The
    /// manifest and not a grant: a capability is enforced where the look is declared, which is
    /// here. A layer nobody can name gets nothing, which is the safe direction.
    fn caps_of(&self, layer: &str) -> Vec<Capability> {
        self.lock()
            .stack
            .layer(layer)
            .map(|l| l.manifest.capabilities.clone())
            .unwrap_or_default()
    }

    /// The rectangle a head is drawing this pane in, recorded before the pane is asked so the
    /// input carries it. `(0, 0)` is a head older than protocol 51, and gets the default size.
    fn record_size(&self, pane: &str, (w, h): (u32, u32)) {
        let size = if w == 0 || h == 0 {
            (SIZE_W, SIZE_H)
        } else {
            (w, h)
        };
        self.lock().sizes.insert(pane.to_string(), size);
    }

    /// Every stack failure not yet said, once each.
    fn say_failures_now(&self, hub: &Arc<Hub>) {
        let mut inner = self.lock();
        let failures: Vec<Failure> = inner.stack.failures().to_vec();
        say_failures(hub, &mut inner, &failures);
    }

    /// The feeds a pane is handed, by the answering layer's own declaration - and only those. A
    /// layer declaring a feed the host does not build is handed no key, not an empty list, which
    /// would say there are none.
    fn feeds_for(&self, hook: &str) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        for want in self.declared_feeds(hook) {
            match want {
                letibot_plugin::pane::Feed::Sessions => {
                    if let Some(source) = self.sessions.as_ref() {
                        out.insert(
                            want.name().to_string(),
                            feed::sessions(&**source, &self.session),
                        );
                    }
                }
                letibot_plugin::pane::Feed::Room => {
                    if let Some(source) = self.room_source() {
                        out.insert(
                            want.name().to_string(),
                            room_feed::room(&*source, &self.session, feed::now_ms()),
                        );
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The feeds the layer that will answer `hook` declared - read off the stack, and only off the
    /// layer [`Stack::call`] will reach.
    fn declared_feeds(&self, hook: &str) -> Vec<letibot_plugin::pane::Feed> {
        self.lock()
            .stack
            .layers()
            .iter()
            .find(|l| {
                !l.disabled
                    && l.manifest.contract == letibot_plugins::CONTRACT
                    && l.manifest.hooks.iter().any(|h| h == hook)
            })
            .map(|l| l.manifest.feeds.clone())
            .unwrap_or_default()
    }

    /// The acts the layer that will answer `hook` declared: the allowlist [`View::validate_for`]
    /// checks a row's `enter` and its letters against, and the words this host will perform.
    fn declared_actions(&self, hook: &str) -> Vec<String> {
        self.lock()
            .stack
            .layers()
            .iter()
            .find(|l| {
                !l.disabled
                    && l.manifest.contract == letibot_plugins::CONTRACT
                    && l.manifest.hooks.iter().any(|h| h == hook)
            })
            .map(|l| l.manifest.actions.clone())
            .unwrap_or_default()
    }

    /// One pane, asked once - and, when the answer asks for a look and this pass may run one
    /// asked again with the look's result. Whatever the answer is, it is published as
    /// `SessionEvent::PluginPane`: a view as the plugin wrote it, or one panel saying why not.
    fn render(&self, hub: &Arc<Hub>, pane: &str, req: Req) {
        let Some((manifest, name)) = pane.split_once('/') else {
            // A pane id is `<manifest name>/<hook name>`; anything else names no hook, so there is
            // nothing to ask and nothing to say.
            return;
        };
        let hook = format!("pane_{name}");
        // Before the lock: `feeds_for` reads the stack, and the mutex is not reentrant, so asking
        // inside this function's own guard is a deadlock that reads as a daemon ignoring panes.
        let feeds = self.feeds_for(&hook);
        // Before the lock for the same reason: `declared_actions` takes the same, non-reentrant
        // stack mutex.
        let declared_actions = self.declared_actions(&hook);
        let mut interior = self.lock();
        let size = interior
            .sizes
            .get(pane)
            .copied()
            .unwrap_or((SIZE_W, SIZE_H));
        let mut state = interior.panes.get(pane).and_then(|p| p.state.clone());
        let mut req = req;
        for step in 0..2 {
            let input = input_value(&state, &req, size, &feeds);
            interior.rec.record(&hook, &input);
            let resolved = interior.stack.call(&hook, &input);
            say_failures(hub, &mut interior, &resolved.failures);
            let Some(answer) = resolved.answer else {
                // A failed call and a hook nobody answers are different screens: a failure says
                // why, while no answer means the layer is gone or disabled - and a pane nobody
                // opened says nothing.
                let was_open = interior.panes.remove(pane).is_some();
                let failure = resolved
                    .failures
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                drop(interior);
                if !failure.is_empty() {
                    hub.publish(SessionEvent::PluginPane {
                        pane: pane.to_string(),
                        view: unavailable(pane, manifest, &failure),
                    });
                } else if was_open {
                    hub.publish(SessionEvent::PluginPaneGone {
                        pane: pane.to_string(),
                    });
                }
                return;
            };
            let by = resolved.by.clone().unwrap_or_else(|| manifest.to_string());
            let (published, view) = match validated(&answer, pane, &by, &declared_actions) {
                Ok(view) => (answer, view),
                Err(why) => {
                    let said = format!("`{pane}`: {why}");
                    if interior.said.insert(said.clone()) {
                        hub.warn("plugin_view_refused", said);
                        interior.notices.push(format!(
                            "the plugin pane `{pane}` (layer `{by}`) answered something the \
                             host's pane contract refuses, so the pane says why instead of \
                             drawing it: {why}"
                        ));
                    }
                    (unavailable(pane, &by, &why), View::default())
                }
            };
            hub.publish(SessionEvent::PluginPane {
                pane: pane.to_string(),
                view: published,
            });
            let open = interior
                .panes
                .entry(pane.to_string())
                .or_insert_with(|| Open {
                    state: None,
                    spec: None,
                    by: by.clone(),
                    every_s: 0,
                    next_at: None,
                });
            open.state = view.state.clone();
            open.spec = view.look.clone();
            open.by = by.clone();
            open.every_s = view.every_s;
            open.next_at =
                (view.every_s > 0).then(|| Instant::now() + Duration::from_secs(view.every_s));
            // One look per pass. A look's result is the next call's input, and an answer that
            // asks for another is what `every_s` schedules - not what a render loops on.
            if step == 0
                && req.may_look
                && let Some(spec) = view.look.clone()
            {
                let caps = interior
                    .stack
                    .layer(&by)
                    .map(|l| l.manifest.capabilities.clone())
                    .unwrap_or_default();
                let result = look::run(&spec, self.look_trees(), &caps);
                state = view.state.clone();
                req = Req {
                    look: Some(result),
                    event: req.event.clone(),
                    may_look: false,
                };
                continue;
            }
            return;
        }
    }
}

// The `plugin` tool's half of this module: scaffold, build, load, stack, disable/enable, test.
// Every one of them asks the LIVE stack, because a second stack would be a second answer to
// *what is in force*, and a layered design cannot afford two of those.

/// A refusal the `plugin` tool hands a model: the sentence saying what did not happen, and the
/// detail it is fixed from - a compiler's own words, the gate's report, the layers there are.
/// Two fields because a failed build needs both: the reason, and the forty lines a model edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub why: String,
    pub detail: String,
}

impl Refused {
    fn new(why: impl Into<String>, detail: impl Into<String>) -> Refused {
        Refused {
            why: why.into(),
            detail: detail.into(),
        }
    }
}

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
            (inner.stack.clone(), inner.rec.clone())
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
        let mut shadow = stack.clone();
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
                match accepted(hook, &answer, &declared) {
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

        // A pass: the live stack gets the candidate, and every screen is brought into line.
        self.lock().stack.apply(cand).map_err(|why| {
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
             no restart.\n\n{}\n\npanes it draws: {}{}",
            said.join("\n"),
            if mine.is_empty() {
                "none (a layer that answers no pane hook)".to_string()
            } else {
                mine.join(", ")
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
                .stack
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
        let (hook, only, label) = {
            let inner = self.lock();
            self.resolve_hook(&inner.stack, what)?
        };
        // The synthetic input is of the HOOK's kind, not a pane's: a `condition` handed a pane's
        // first frame answers nothing and reads as a broken layer.
        let input = input
            .cloned()
            .unwrap_or_else(|| synthetic_for(&hook, &self.feeds_for(&hook)));
        let (stack, rec) = {
            let inner = self.lock();
            (inner.stack.clone(), inner.rec.clone())
        };
        let asked = match &only {
            None => stack,
            Some(id) => {
                // Every other DISK layer off: a built-in is the floor and stays, because a layer
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
                match accepted(&hook, answer, &declared) {
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

/// C3's input, once, for a caller that has none of its own: the first frame of any pane - nothing
/// looked at, nothing pressed, no state.
fn synthetic_pane_input(feeds: &BTreeMap<String, Value>) -> Value {
    input_value(&None, &Req::fresh(), (SIZE_W, SIZE_H), feeds)
}

/// The first frame of any hook, for `plugin test` and for `load`'s replay of a layer nobody has
/// asked yet: the SDK's own synthetic example of that kind. A layer that cannot answer it traps on
/// its own first frame, which is the cheapest defect to find.
fn synthetic_for(hook: &str, feeds: &BTreeMap<String, Value>) -> Value {
    use letibot_plugin::{condition, notice, tool_call, tool_result};
    let value = if hook.starts_with("pane_") {
        return synthetic_pane_input(feeds);
    } else if hook == tool_call::HOOK {
        serde_json::to_value(tool_call::ToolCallIn::synthetic())
    } else if hook == tool_result::HOOK {
        serde_json::to_value(tool_result::ToolResultIn::synthetic())
    } else if hook.starts_with(condition::PREFIX) {
        serde_json::to_value(condition::ConditionIn::synthetic())
    } else {
        serde_json::to_value(notice::Notice::synthetic())
    };
    value.unwrap_or(Value::Null)
}

/// Would the host accept this answer for `hook`? - the same parse the running host does, in one
/// place, so `plugin test` and `load` cannot disagree with what a session would live with. `Ok` is
/// the one line a person reads (what the answer would do); `Err` is the refusal.
fn accepted(hook: &str, answer: &Value, declared: &[String]) -> Result<String, String> {
    use letibot_plugin::{condition, notice, tool_call, tool_result};
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
                format!("it answers the call without running it: {text}")
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
/// result, or a watch - the fields the reader needs to recognise WHICH input went wrong.
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

impl PaneDriver for PluginHost {
    fn attach(&self, _session_id: &str, hub: &Arc<Hub>) {
        // A head arriving is the first moment a built-in pane is worth a compile, but the compile
        // costs seconds, so it is only asked for here and runs on the next tick's thread.
        self.builtins_wanted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.announce(hub);
    }

    fn open(&self, _session_id: &str, hub: &Arc<Hub>, pane: &str, size: (u32, u32)) {
        // A head that asks for a pane without having been announced one still gets it.
        self.ensure_builtin_panes();
        self.record_size(pane, size);
        self.render(hub, pane, Req::fresh());
    }

    /// The pane is off the screen, so it comes off the refresh clock. The view goes with the entry
    /// and the next open asks for a fresh one; the rectangle stays, so a reopen on the same screen
    /// does not fall back to the default for one frame.
    fn close(&self, _session_id: &str, pane: &str) {
        self.lock().panes.remove(pane);
    }

    fn action(&self, _session_id: &str, hub: &Arc<Hub>, pane: &str, item: &str, action: &str) {
        // `look` re-runs the look now, because that is what the key meant: the file read again
        // not the pane called with its old answer.
        if action == "look" {
            let (spec, by) = {
                let inner = self.lock();
                match inner.panes.get(pane) {
                    Some(open) => (open.spec.clone(), open.by.clone()),
                    None => (None, String::new()),
                }
            };
            let caps = self.caps_of(&by);
            self.render(
                hub,
                pane,
                Req {
                    look: spec.map(|spec| look::run(&spec, self.look_trees(), &caps)),
                    event: Some((item.to_string(), action.to_string())),
                    may_look: true,
                },
            );
            return;
        }
        // A room pane's keys are the room's own verbs and this host performs them: what a claim
        // does is the room's state machine, one process away, so the act is taken here and not
        // handed back to the layer. `View::validate_for` has already gated the row's words.
        if let Some(room) = self.room_source().as_ref()
            && pane_hook(pane) == ROOM_HOOK
        {
            // The fifth control is not a room verb: a restart is a daemon lifetime, asked of the
            // supervisor that starts coordinators, because the pane is fed by the instance it
            // would kill. Both outcomes are said, since a silent control is one nobody can trust.
            if action == RESTART_VERB {
                // Off this thread, because it is the head's own connection: a launcher that
                // overran its minute would leave the person's prompts and keys queued behind it.
                match crate::room::supervise::InFlight::take() {
                    None => {
                        let _ = hub.warn(
                            "room_restart_refused",
                            "the room was not restarted: a restart is already running here — a box \
                             has one room, and two stops and two starts is not a restart",
                        );
                    }
                    Some(running) => {
                        let room = Arc::clone(room);
                        let hub = Arc::clone(hub);
                        let _ = std::thread::Builder::new()
                            .name("room-restart".to_string())
                            .spawn(move || {
                                let _running = running;
                                // Two literal codes, not one variable: a code that arrives
                                // through a variable is one no reader's table can check.
                                match room.restart() {
                                    Ok(said) => {
                                        let _ = hub.warn("room_restarted", said);
                                    }
                                    Err(why) => {
                                        let _ = hub.warn(
                                            "room_restart_refused",
                                            format!("the room was not restarted: {why}"),
                                        );
                                    }
                                }
                            });
                    }
                }
            } else if let Err(why) = room.act(item, action) {
                // A refusal is said, not swallowed: a key that does nothing is indistinguishable
                // from a key nobody pressed, and the room's sentence names who holds the tag.
                hub.warn("room_act_refused", format!("`{action}` on `{item}`: {why}"));
            }
        }
        self.render(
            hub,
            pane,
            Req {
                look: None,
                event: Some((item.to_string(), action.to_string())),
                may_look: false,
            },
        );
    }
}

/// The hook whose rows are the room's four controls. The match is on the hook and not the layer's
/// name: what makes a key a room verb is what the pane draws, so a project shipping its own layer
/// over `pane_room` is drawing the same room.
pub const ROOM_HOOK: &str = "room";

/// The pane's fifth control, and the one verb here that is not the room's: `claim`, `release`
/// `abandon` and `rule` are the room's state machine, while `restart` is a daemon lifetime asked of
/// the supervisor. It is declared in the manifest like the other four, so `validate_for` gates it.
pub const RESTART_VERB: &str = "restart";

/// The hook part of a pane id (`<manifest name>/<hook name>`, without the `pane_` prefix).
fn pane_hook(pane: &str) -> &str {
    pane.split_once('/').map(|(_, hook)| hook).unwrap_or("")
}

/// The pane ids a stack's layers export, in stack order, each once.
fn pane_names(stack: &Stack) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for layer in stack.layers() {
        if layer.disabled || layer.manifest.contract != letibot_plugins::CONTRACT {
            continue;
        }
        for hook in &layer.manifest.hooks {
            let Some(name) = hook.strip_prefix("pane_") else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let pane = format!("{}/{name}", layer.manifest.name);
            if !out.contains(&pane) {
                out.push(pane);
            }
        }
    }
    out
}

/// A pane's input: the state it returned, the look's result, the key pressed, the rectangle, and
/// the feeds its layer declared. Every field is present, because a plugin deserialises this into a
/// struct and a missing key cannot be told from an empty one.
fn input_value(
    state: &Option<Value>,
    req: &Req,
    (w, h): (u32, u32),
    feeds: &BTreeMap<String, Value>,
) -> Value {
    let look = req.look.clone().unwrap_or(Value::Null);
    let event = match &req.event {
        Some((item, action)) => json!({"item": item, "action": action}),
        None => Value::Null,
    };
    json!({
        "v": PANE_V,
        "state": state.clone().unwrap_or(Value::Null),
        "look": look,
        "event": event,
        "size": {"w": w, "h": h},
        "feeds": feeds,
    })
}

/// The plugin's answer, as a view this host can drive - or the sentence saying why not.
/// `validate_for` is the check that matters here: a row asking for a verb the layer never declared
/// is refused before it is drawn, because the daemon is what performs those verbs.
fn validated(answer: &Value, pane: &str, by: &str, declared: &[String]) -> Result<View, String> {
    let view: View = serde_json::from_value(answer.clone()).map_err(|e| {
        format!("the answer is not a C3 view ({e}), while `{pane}` was resolved from `{by}`")
    })?;
    view.validate_for(declared)?;
    Ok(view)
}

/// A pane that could not answer, as a pane that says so: an empty dashboard and a broken plugin
/// look identical on a screen.
fn unavailable(pane: &str, layer: &str, why: &str) -> Value {
    let view = View {
        title: pane.to_string(),
        panels: vec![Panel {
            id: "pane".to_string(),
            // One lowercase letter the host does not own, so a view this host made itself passes
            // the same `validate` a plugin's does.
            key: "u".to_string(),
            title: "pane".to_string(),
            summary: vec![Block::Lines(vec![Line {
                label: "answer".to_string(),
                value: Some("refused".to_string()),
                role: Some(Role::Failure),
                ..Line::default()
            }])],
            unavailable: Some(Unavailable {
                why: why.to_string(),
                fix: format!("the layer {layer} answered something the host refused"),
            }),
            ..Panel::default()
        }],
        ..View::default()
    };
    serde_json::to_value(&view).unwrap_or(Value::Null)
}

/// Say each stack failure once and queue it for the model, which is the one that can fix a layer -
/// which is what makes the fall-through loud rather than silent.
fn say_failures(hub: &Arc<Hub>, inner: &mut Inner, failures: &[Failure]) {
    for failure in failures {
        let said = failure.to_string();
        if !inner.said.insert(said.clone()) {
            continue;
        }
        inner.notices.push(format!(
            "a plugin layer failed and the layer below it answered: {said}"
        ));
        hub.warn("plugin_layer", said);
    }
}

/// Where a session's own layers live: `<data dir>/sessions/<id>/plugins`. One function because
/// three callers must agree - discovery, `plugin scaffold` and the daemon's host - and a
/// disagreement is a plugin written where nothing discovers it. `None` when the data dir is unknown.
pub fn session_plugins_dir(session: &str) -> Option<PathBuf> {
    crate::mergequeue::git::letibot_data_dir()
        .map(|dir| dir.join("sessions").join(session).join("plugins"))
}

/// `$XDG_CONFIG_HOME/letibot`, else `~/.config/letibot` - where the user scope's layers are.
fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|base| base.join("letibot"))
}

/// The tools this session's layers declare, in precedence order, one per tool name. Read at
/// `Harness::open` - a declared tool is prompt bytes - and only from `plugin.toml`s: the built-in
/// scope is what the binary carries, not what this session asked for.
pub fn declared_tools(session: &str, workspace: &Path) -> Vec<DeclaredTool> {
    let mut all = Stack::declared_tools(
        session_plugins_dir(session).as_deref(),
        Some(workspace),
        user_config_dir().as_deref(),
    );
    let mut seen = BTreeSet::new();
    all.retain(|d| seen.insert(d.tool.name.clone()));
    all
}

/// What one hook's answer was, and the layer it came from - the by-line a rewrite is reported
/// with.
#[derive(Debug, Clone, PartialEq)]
pub struct Answered<T> {
    pub out: T,
    pub by: String,
}

/// The `monitor` call a layer's `arm` becomes - the same door the model's own calls use, so the
/// coalescing, the already-watched refusal and the TTL caps stay the tool's. `repeat` is on, as the
/// harness's own pull-request watch is: a watch for one push's checks is worth having for the next.
pub fn arm_call(arm: &letibot_plugin::tool_result::Arm) -> letibot_transcript::ToolCall {
    let name = arm.plugin.clone();
    let arguments = serde_json::json!({
        "name": name,
        "plugin": arm.plugin,
        "args": arm.args,
        "repeat": true,
    });
    letibot_transcript::ToolCall {
        id: format!("plugin-arm-{name}"),
        name: "monitor".into(),
        arguments: arguments.to_string(),
    }
}

/// The layer that answers a hook, as a look needs it: its id, its manifest name, its scope, and
/// what it may look at. A name and an id because two readers want different ones - mechanisms use
/// the id (`s3`), and a sentence to a model carries the name.
pub struct Answering {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub capabilities: Vec<Capability>,
    /// Whether this layer runs freely: a built-in, or a layer whose bytes a person pinned. An
    /// `exec` look of an unpinned layer goes through the gate; of a free one, it runs.
    pub trusted: bool,
}

/// The same layer, as a sentence a model reads: `name (scope)`.
impl std::fmt::Display for Answering {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.scope)
    }
}

impl PluginHost {
    /// The layer that answers `hook`: the topmost enabled one that exports it, which is the one the
    /// stack asks first. A look a lower layer declared runs only when the upper one asked below
    /// and the upper one's capabilities are still the ones in force for the answer that came back.
    pub fn answering(&self, hook: &str) -> Option<Answering> {
        let inner = self.lock();
        inner
            .stack
            .layers()
            .iter()
            .find(|l| !l.disabled && l.manifest.hooks.iter().any(|h| h == hook))
            .map(|l| Answering {
                id: l.id.clone(),
                name: l.manifest.name.clone(),
                scope: l.scope,
                capabilities: l.manifest.capabilities.clone(),
                trusted: l.trusted,
            })
    }

    /// The capabilities the layer that answers `hook` declared - what a look runs under.
    pub fn capabilities_for(&self, hook: &str) -> Vec<Capability> {
        self.answering(hook)
            .map(|l| l.capabilities)
            .unwrap_or_default()
    }

    /// The layer `id`, as the sentence a model reads: its manifest name and scope, `pr-watch
    /// (project)`. `s3` is a layer's identity to every mechanism and nothing to a reader, and a
    /// layer nobody can find is answered by its id rather than an invented name.
    pub fn layer_sentence(&self, id: &str) -> String {
        let inner = self.lock();
        match inner.stack.layer(id) {
            Some(l) => format!("{} ({})", l.manifest.name, l.scope),
            None => id.to_string(),
        }
    }

    /// A look the gate would not run, said once - per layer, program and reason, since the sentence
    /// names all three and is its own dedup key. A refusal repeated on every firing is how a model
    /// learns to skip them; one that changes is news again.
    pub fn look_refused(&self, hub: &Arc<Hub>, said: String) {
        {
            let mut inner = self.lock();
            if !inner.said.insert(said.clone()) {
                return;
            }
            inner.notices.push(said.clone());
        }
        hub.warn("plugin_look", said);
    }

    /// The last inputs this session's host asked `hook` with, newest last - what the gate's replay
    /// runs over. Public because the record is the session's, not this module's: a head, `plugin
    /// test`'s replay and a test all read the same ring.
    pub fn recorded(&self, hook: &str) -> Vec<Value> {
        self.lock().rec.inputs(hook).to_vec()
    }

    /// The tree a look resolves against - the session's own workspace.
    pub fn workspace(&self) -> PathBuf {
        self.workspace.clone()
    }

    /// One result through the `on_tool_result` hook: the call, how it ended, and what the model
    /// would read. `None` when nothing answers, or the answer is a pass or a mask; an answer that is
    /// not the contract is refused and the call's own result stands.
    pub fn on_tool_result(
        &self,
        hub: &Arc<Hub>,
        call: &letibot_plugin::Call,
        outcome: &str,
        payload: &str,
    ) -> Option<Answered<letibot_plugin::tool_result::ToolResultOut>> {
        use letibot_plugin::tool_result::{ToolResultIn, ToolResultOut};
        let input = ToolResultIn::new(call.clone(), outcome, payload);
        let input = serde_json::to_value(&input).ok()?;
        let resolved = self.ask(hub, letibot_plugin::tool_result::HOOK, &input)?;
        let answer = resolved.answer?;
        let by = resolved.by.clone().unwrap_or_default();
        match serde_json::from_value::<ToolResultOut>(answer.clone()) {
            Ok(out) => Some(Answered { out, by }),
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a tool result — \
                         `payload`, `append` and `arm` are the whole of it: {why}. Its answer \
                         was {answer}, so the call's own result stands"
                    ),
                );
                None
            }
        }
    }
}

impl PluginHost {
    /// One call through the `on_tool_call` hook - before the gate, so what a layer rewrites is what
    /// the gate then adjudicates. `None` when nothing answers; an answer that is not the contract is
    /// refused and the call runs as proposed, because the gate still decides.
    pub fn on_tool_call(
        &self,
        hub: &Arc<Hub>,
        call: &letibot_plugin::Call,
        standing: &letibot_tools::runtime::Standing,
    ) -> Option<Answered<letibot_plugin::tool_call::ToolCallOut>> {
        use letibot_plugin::tool_call::{Session, ToolCallIn, ToolCallOut};
        let session = Session {
            session: standing.session.clone(),
            mode: standing.mode.clone(),
            // The host's own workspace, not the gate's string: this is the directory a layer is
            // written about, and it is the same one the looks and the panes resolve against.
            workspace: self.workspace.display().to_string(),
        };
        let input = ToolCallIn::new(call.clone(), session);
        let input = serde_json::to_value(&input).ok()?;
        let resolved = self.ask(hub, letibot_plugin::tool_call::HOOK, &input)?;
        // A guard that failed refuses the call: the layer's id and why go to the model.
        if let Some((by, why)) = resolved.refused {
            let said = format!(
                "plugin layer `{by}` guards this call and failed, so the call is refused: {why}"
            );
            self.complain(hub, &by, &said);
            return Some(Answered {
                out: ToolCallOut::refuse(said),
                by,
            });
        }
        let answer = resolved.answer?;
        let by = resolved.by.clone().unwrap_or_default();
        let refuse = |this: &Self, why: String| {
            let said = format!(
                "plugin layer `{by}` answered badly for a guard, so the call is refused: {why}"
            );
            this.complain(hub, &by, &said);
            Some(Answered {
                out: ToolCallOut::refuse(said),
                by: by.clone(),
            })
        };
        match serde_json::from_value::<ToolCallOut>(answer.clone()) {
            Ok(out) => {
                if let letibot_plugin::tool_call::Verdict::Rewrite(args) = out.verdict()
                    && let Err(why) = only_smaller(&call.arguments, args)
                {
                    return refuse(self, why);
                }
                Some(Answered { out, by })
            }
            Err(why) => refuse(
                self,
                format!(
                    "its answer was {answer}, which is not a call's verdict (`pass`, `arguments`, \
                     `refuse` and `answer` are the whole of it): {why}"
                ),
            ),
        }
    }

    /// One call through the `adjudicate_` floor: the names a layer gives the action and the rung it
    /// requires, or `Pass` when nothing answered. A trap or a bad answer is `Trapped`, which the gate
    /// reads as the strictest tier - a floor that cannot answer has not said the call is fine.
    pub fn adjudicate(
        &self,
        hub: &Arc<Hub>,
        action: &letibot_plugin::adjudicate::Action,
    ) -> letibot_tools::adjudicate::Floored {
        use letibot_plugin::adjudicate::{AdjudicateIn, AdjudicateOut, Floor};
        use letibot_tools::adjudicate::Floored;
        let input = AdjudicateIn::new(action.clone());
        let Ok(input) = serde_json::to_value(&input) else {
            return Floored::Trapped {
                by: String::new(),
                why: "the action this host built is not expressible as JSON".into(),
            };
        };
        let Some(resolved) = self.ask(hub, letibot_plugin::adjudicate::HOOK, &input) else {
            return Floored::Pass;
        };
        if let Some((by, why)) = resolved.refused {
            let said = format!("it could not answer, so the call is at the strictest tier: {why}");
            self.complain(hub, &by, &said);
            return Floored::Trapped { by, why };
        }
        let Some(answer) = resolved.answer else {
            return Floored::Pass;
        };
        let by = self.layer_sentence(&resolved.by.unwrap_or_default());
        match serde_json::from_value::<AdjudicateOut>(answer.clone()) {
            // A layer that names nothing and asks for nothing is silence, not an answer.
            Ok(out) if out.says_nothing() => Floored::Pass,
            Ok(out) => {
                // Read before the names move: the rung is what the gate acts on, the names do not.
                let floor = out.required().unwrap_or(Floor::Auto);
                Floored::Raised {
                    intents: out.intents,
                    floor,
                    by,
                }
            }
            Err(why) => {
                let said = format!(
                    "its answer was {answer}, which is not a floor (`intents` and `floor` are the \
                     whole of it): {why}"
                );
                self.complain(hub, &by, &said);
                Floored::Trapped {
                    by,
                    why: format!("its answer is not the contract: {why}"),
                }
            }
        }
    }
}

/// A rewrite may only make a call smaller: the tool is fixed by the contract, and no argument
/// may appear that the model did not send.
fn only_smaller(was: &serde_json::Value, now: &serde_json::Value) -> Result<(), String> {
    match (was, now) {
        (_, serde_json::Value::Null) => Ok(()),
        (serde_json::Value::Object(was), serde_json::Value::Object(now)) => {
            match now.keys().find(|k| !was.contains_key(*k)) {
                Some(k) => Err(format!(
                    "its rewrite adds the argument `{k}`, and a layer can only make a call smaller"
                )),
                None => Ok(()),
            }
        }
        _ => Err("its rewrite changes the shape of the arguments, and a layer can only make a call smaller".into()),
    }
}

/// The runtime's `on_tool_call`, backed by one session's plugin host. A slot and not the host
/// itself: the runtime is built when the harness opens, the host when the daemon opens the session
/// so a session the daemon does not serve leaves the slot empty and its calls run as they did.
pub struct CallHooks {
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
}

impl CallHooks {
    pub fn new(slot: Arc<Mutex<Option<Arc<PluginHost>>>>, hub: Arc<Hub>) -> CallHooks {
        CallHooks { slot, hub }
    }
}

impl letibot_tools::runtime::CallHook for CallHooks {
    fn before(
        &self,
        call: &letibot_plugin::Call,
        standing: &letibot_tools::runtime::Standing,
    ) -> Option<letibot_tools::runtime::Patched> {
        let host = self
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let answered = host.on_tool_call(&self.hub, call, standing)?;
        // An `answer` is somebody else's words standing in for a result, and the payload cannot
        // say so: it reads like a tool's own output. So the result carries one line naming the
        // layer as a person wrote it. A `refuse` needs none - its outcome already says so.
        let note = matches!(
            answered.out.verdict(),
            letibot_plugin::tool_call::Verdict::Answer(_)
        )
        .then(|| {
            format!(
                "answered by plugin {}; the tool did not run",
                host.layer_sentence(&answered.by)
            )
        });
        Some(letibot_tools::runtime::Patched {
            out: answered.out,
            by: answered.by,
            note,
        })
    }
}

/// The gate's `adjudicate_` floor, backed by one session's plugin host. A slot rather than the host
/// for the reason [`CallHooks`] holds one: the gate is built when the harness opens and the host
/// when the daemon opens the session, so a session nobody serves decides exactly as it did.
pub struct FloorHooks {
    slot: Arc<Mutex<Option<Arc<PluginHost>>>>,
    hub: Arc<Hub>,
}

impl FloorHooks {
    pub fn new(slot: Arc<Mutex<Option<Arc<PluginHost>>>>, hub: Arc<Hub>) -> FloorHooks {
        FloorHooks { slot, hub }
    }
}

impl letibot_tools::adjudicate::FloorHook for FloorHooks {
    fn floor(
        &self,
        action: &letibot_plugin::adjudicate::Action,
    ) -> letibot_tools::adjudicate::Floored {
        let Some(host) = self.slot.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return letibot_tools::adjudicate::Floored::Pass;
        };
        host.adjudicate(&self.hub, action)
    }
}

impl PluginHost {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Where a write's provenance goes, on the daemon's side of `letibot_tools::provenance::Sink`. It
/// filters to what is about a layer and appends to the trust book, so *who wrote these bytes* is a
/// fact on disk. The book is its own `Arc<Trust>` and not the host's: they share the FILE.
pub struct Provenance {
    trust: Arc<letibot_plugins::Trust>,
    session: String,
}

impl Provenance {
    pub fn new(session: impl Into<String>) -> Provenance {
        Provenance {
            trust: letibot_plugins::Trust::open_default(),
            session: session.into(),
        }
    }

    /// The same, over a book a test hands in.
    pub fn over(trust: Arc<letibot_plugins::Trust>, session: impl Into<String>) -> Provenance {
        Provenance {
            trust,
            session: session.into(),
        }
    }
}

impl letibot_tools::provenance::Sink for Provenance {
    fn wrote(&self, path: &std::path::Path, bytes: &[u8]) {
        self.trust.note_write(path, bytes, &self.session);
    }
}
