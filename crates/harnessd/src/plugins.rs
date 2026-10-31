//! **The plugin layers a session has, and the panes they answer** — W4 of
//! `docs/plugins-slice1.md`: the wiring between the daemon's sessions and the stack
//! `crates/plugins` implements (C2) and the pane document `crates/sessionlog` owns (C3, C4).
//!
//! # What is here, and what is not
//!
//! | piece | is |
//! |---|---|
//! | [`PluginHost`] | one session's [`letibot_plugins::Stack`], its [`letibot_plugins::Recorder`], the panes a head has opened, and the clock a hot reload rides |
//! | [`rano::RanoLoader`] | the one `Loader` production uses: rano compiles the module, this answers a hook |
//! | [`look`] | `{"read": path}` — the host does the I/O, against the workspace, under the layer's `read` capability |
//!
//! Not here, deliberately: the answer protocol, the precedence and the gate. Those are
//! `letibot_plugins`' and are tested there with fakes; this module is the daemon's half — which
//! session has which stack, when a look runs, and what reaches a head.
//!
//! # Why a host per session
//!
//! The layers are discovered per session (`docs/plugins-plan.md` §2.1): the session's own dir,
//! its project's `.letibot/plugins`, and the user's. So two sessions on one daemon have two
//! stacks, and a pane id (`plan-dashboard/dashboard`) is one session's.
//!
//! # The rules this file keeps from the contracts
//!
//! * **A pane's answer is data until the host validates it.** A view that fails
//!   `sessionlog::pane::View::validate` — or an answer that is not a view at all — is published as
//!   ONE panel saying *why* and *what fixes it* (C3's rule, and W4's brief): never silence,
//!   because an empty dashboard and a broken plugin look identical on a screen and mean opposite
//!   things.
//! * **A look is the host's act.** Slice 1 runs `read` and refuses `exec` ([`look::EXEC_REFUSED`]),
//!   and never lets the module touch the filesystem itself.
//! * **Only a gated candidate is applied.** [`letibot_plugins::Stack::apply`] is handed the same
//!   `Candidate` the gate replayed (`crates/plugins/src/gate.rs`'s own note), and a refusal keeps
//!   the old layer and tells the session's model what the report said.
//! * **The gate's build never runs on the worker.** The tick only stats files; the gate — which
//!   runs `cargo` — is a thread of its own.
//!
//! # What slice 1 does not have, and why it is written down
//!
//! **No look runs for a pane nobody opened.** [`PluginHost::announce`] publishes each pane's first
//! frame with `may_look: false`, so a dashboard on a session's list costs a wasm call and no file
//! read until a head opens it. The refresh clock (`every_s`) exists only for panes on that list
//! that a head has opened, and `ClientFrame::PaneClose` (protocol 51) takes a pane off it — a
//! closed pane is a pane the host stops asking.

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
use serde_json::{Value, json};

pub mod conditions;
pub mod contract;
pub mod desk;
pub mod feed;
pub mod look;
pub mod rano;
pub mod room_feed;
pub mod scaffold;
// **A tool a layer declares, and the looks its hook asks the host to run** — `tool_` in
// `docs/plugins-plan.md` §4.8. It is a module rather than a section of this file because it is a
// `Tool` (it goes in the registry beside `read` and `bash`) and because the three look primitives
// it runs are a filesystem API of their own.
pub mod tool;

/// **How often the hot-reload tick looks** — `docs/plugins-plan.md` §2.3 makes a change take
/// effect on the next hook call, and this is the longest a person waits after saving one. Two
/// seconds is `mergequeue::watch`'s own poll, for the same reason: a stat per layer is free.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// **The rectangle a pane draws in when the head did not say one.** C4's `PaneOpen` carries the
/// pane's own `w` and `h` (protocol 51); a head built before that sends neither, and a dashboard
/// laid out for a wide screen is the honest default rather than a guess about a terminal.
pub const SIZE_W: u32 = 120;
pub const SIZE_H: u32 = 40;

/// The contract version every pane input carries (C3): the host says which one it speaks.
///
/// **It is the SDK's own number and not a copy.** It said `1` while the contract had grown to 2
/// (`feeds`, `folded`, a row's `enter`), so a layer that read `v` to decide whether it would be
/// handed a feed was told *no* by a host that was about to hand it one.
const PANE_V: u32 = letibot_plugin::pane::V;

/// One session's layers, its recorded inputs, and the panes a head has opened.
pub struct PluginHost {
    session: String,
    workspace: PathBuf,
    /// **The session's own plugin directory** — where `plugin scaffold` writes and where a
    /// session-scope layer is discovered from (C2's session scope). Kept beside the stack
    /// because the stack only knows the layers it FOUND, and a scaffold's job is the directory a
    /// layer will be written into before any discovery can see it.
    session_dir: Option<PathBuf>,
    inner: Mutex<Inner>,
    /// How the gate's step 1 is performed. `CargoBuilder` in the daemon; a table of answers in a
    /// test, because a test that ran cargo would need the wasm target and a real crate to build.
    builder: Arc<dyn Builder>,
    /// **Where a pane's `sessions` feed comes from** — the daemon's own registry, set by
    /// [`PluginHost::with_sessions`] at the one place the daemon builds a host
    /// (`Sessions::open_plugins`). `None` in a host a test built, and then a layer that declares
    /// the feed is handed no key at all rather than an empty list (see [`feed`]).
    sessions: Option<Arc<dyn feed::SessionFeed>>,
    /// **Where a pane's view of the room comes from** — this session's own connection to the box's
    /// coordinator (`crate::room::link::SessionRoom`), set by [`PluginHost::with_room`] at the one
    /// place the daemon builds a host. `None` in a host a test built, and on a box with no
    /// coordinator; a layer that declares the `room` feed is then handed **no key at all** rather
    /// than an empty room (see [`room_feed`]).
    room: Option<Arc<dyn room_feed::RoomFeed>>,
}

struct Inner {
    stack: Stack,
    rec: Recorder,
    /// The panes a head has opened, by id, with what the pane returned and when it wants asking
    /// again.
    panes: BTreeMap<String, Open>,
    /// **The rectangle each pane was last asked to draw in**, by pane id — `ClientFrame::PaneOpen`'s
    /// `w` and `h`, which only a head knows. Kept beside `panes` rather than inside an `Open`
    /// because a close is not a forget: a pane opened again on the same screen should draw at the
    /// size it was last drawn at, and the entry it would have lived in is exactly what a close
    /// removes.
    sizes: BTreeMap<String, (u32, u32)>,
    /// Layer ids with a gate running right now, so a poll that sees the same change twice does
    /// not compile it twice.
    gating: BTreeSet<String>,
    /// What has already been said on the session's log. A failure said once is news; the same
    /// failure said every two seconds is noise a model learns to ignore.
    said: BTreeSet<String>,
    /// What the session's MODEL has not been told yet. Drained by the worker
    /// ([`PluginHost::take_notices`]) because a notice is a transcript row, and only a harness can
    /// write one of those.
    notices: Vec<String>,
    /// When the next hot-reload poll is due.
    poll_at: Instant,
    /// **Pins whose bytes moved before this host existed** — `with_trust`'s answer, held for the
    /// first tick that can say them. See [`PluginHost::with_trust`].
    broken_pins: Vec<letibot_plugins::BrokenPin>,
}

/// What the host knows about one open pane.
struct Open {
    /// The `state` the pane answered with, handed back to it on the next call — §1, principle 5:
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
    /// **Discover a session's layers**, from the three scopes C2 names.
    ///
    /// `session_dir` is the session's own plugin directory and `user_dir` the letibot config dir
    /// (`<user_dir>/plugins` is read from it); `workspace` is the project root, whose
    /// `.letibot/plugins` is the project scope and whose files a `read` look resolves against.
    /// Every one of them may be absent, which is an empty scope and not an error.
    pub fn discover(
        session: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_dir: Option<PathBuf>,
        user_dir: Option<PathBuf>,
        loader: Arc<dyn Loader>,
        builder: Arc<dyn Builder>,
    ) -> PluginHost {
        let workspace = workspace.into();
        let stack = Stack::discover(
            session_dir.as_deref(),
            Some(&workspace),
            user_dir.as_deref(),
            loader,
        );
        PluginHost {
            session: session.into(),
            workspace,
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
            room: None,
        }
    }

    /// **Hand this host the daemon's session list**, so a pane that declares the `sessions` feed is
    /// given one. A builder rather than a constructor argument because it is the daemon's fact and
    /// not a layer's: a host built by a test has no list and says so by handing nothing.
    pub fn with_sessions(mut self, source: Arc<dyn feed::SessionFeed>) -> PluginHost {
        self.sessions = Some(source);
        self
    }

    /// **Hand this host the session's own connection to the box's room**, so a pane that declares
    /// the `room` feed is given one and its four controls have somewhere to go.
    ///
    /// A builder for the same reason [`PluginHost::with_sessions`] is: which room a session is in is
    /// the daemon's fact, and a host a test built has none and says so by handing nothing.
    pub fn with_room(mut self, source: Arc<dyn room_feed::RoomFeed>) -> PluginHost {
        self.room = Some(source);
        self
    }

    /// [`PluginHost::discover`] with the two dirs this box's conventions give it: the session's own
    /// state dir under the letibot data dir, and `$XDG_CONFIG_HOME/letibot`.
    ///
    /// The session scope is `<data dir>/sessions/<id>/plugins`, where a session's own layers would
    /// be written (C2's "the session's own plugin directory"). It does not exist until something
    /// writes one, and a scope that does not exist is empty.
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
        // **D17's book, read once at discovery** and re-read on the poll: a placed layer is free
        // only when a person recorded a hash of these exact bytes. A host built with
        // `discover` and no book (a test standing a stack up) has every placed layer unpinned,
        // which is the fail-closed direction.
        .with_trust(letibot_plugins::Trust::open_default())
    }

    /// **Hand this host D17's trust book** — which placed layers a person has pinned, and which
    /// ones have changed since. See [`letibot_plugins::Trust`].
    ///
    /// **A pin that broke while no daemon was running is news, and this is where it arrives.**
    /// `Stack::set_trust` re-decides every layer and answers the pins whose bytes moved — and it
    /// records them as said as it answers them, so a caller that dropped the answer would silence
    /// the warning for exactly the case it exists for. So they are queued, and the first
    /// [`PluginHost::tick`] says them (`plugin_untrusted`). Nothing runs free in the meantime: the
    /// refusal sentences already name both hashes.
    pub fn with_trust(self, trust: Arc<letibot_plugins::Trust>) -> PluginHost {
        let news = self.lock().stack.set_trust(trust);
        if !news.is_empty() {
            self.lock().broken_pins.extend(news);
        }
        self
    }

    /// **What D17's book says about a layer this host holds** — by id, by manifest name, or by
    /// the directory it sits in. `None` when this stack has no such layer.
    ///
    /// A clone rather than a reference because the stack lives behind the host's mutex, and the
    /// caller that needs this (`PluginTool`, for the sentence a refusal carries) is holding it
    /// across a call.
    pub fn layer(&self, what: &str) -> Option<letibot_plugins::Layer> {
        self.lock().stack.layer_named(what).cloned()
    }

    /// [`PluginHost::layer`] with the SCOPE it must be — for a caller that has one.
    ///
    /// A tool's `DeclaredTool` names its layer by directory name, and two scopes may hold layers
    /// of one name (a session fix and the project layer it was promoted from). A name alone is
    /// then two answers to *is this layer pinned*, so the caller that knows the scope says it;
    /// the fallback is the name-only lookup, because a layer that has moved scope (a project
    /// layer promoted from a session) is still the layer the declaration meant.
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

    /// **Whether the layer answering `hook` runs freely** — a built-in, or a layer whose bytes a
    /// person pinned. Asked by an `exec` look before the gate: D17's first two arms.
    pub fn free_layer(&self, hook: &str) -> bool {
        self.answering(hook).is_some_and(|a| a.trusted)
    }

    /// This host as the daemon uses it: rano loads the modules and cargo builds them.
    ///
    /// # And the built-ins that answer a hook the DAEMON dispatches
    ///
    /// A head installs the whole built-in table itself (`letibot_builtins::install`, in the TUI's
    /// own stack) because the built-ins are mostly panes, and a pane is the head's to draw. The
    /// daemon's stack is the one a `tool_` hook is resolved through, so it needs the built-in
    /// layers that declare a tool — and only those: installing the pane ones here as well would
    /// announce every pane twice, once from each stack.
    ///
    /// **And only the ones this session DECLARED a tool from.** `crate::plugins::declared_tools`
    /// is the same read the prompt was composed from, so the two cannot disagree about what this
    /// session asked for, and a session that asked for nothing installs nothing. That is not
    /// tidiness: MEASURED 2026-10-28, compiling this binary's tool layer costs **1.97 s** of CPU in
    /// a debug host, and a session that seats no layer tool can never call it — the `PluginTool`
    /// that would is what the declaration seated. Installing it anyway cost every session open two
    /// seconds and made `letibot-bridge`'s `no_spin` window all compile.
    ///
    /// A built-in whose module the engine refuses is recorded in the stack's own failure list and
    /// said to the model by `say_failures`, so a broken built-in is a sentence rather than a tool
    /// that silently is not there.
    pub fn production(session: &str, workspace: &Path) -> PluginHost {
        let host = PluginHost::for_session(
            session,
            workspace,
            Arc::new(rano::RanoLoader),
            Arc::new(CargoBuilder),
        );
        host.install_builtin_tools(&crate::plugins::declared_tools(session, workspace));
        host
    }

    /// **Put the built-in layers a session declared a tool from at the bottom of this stack.**
    ///
    /// The match is on the tool's NAME: a built-in that declares `claude_transcript` is installed
    /// for a session whose declarations include `claude_transcript`, and not otherwise. The layer
    /// above it is what the model was offered — the built-in is the module that answers, which is
    /// why a project's ask can be a manifest with no `plugin.wasm`.
    ///
    /// **A built-in whose PANE this daemon answers is not installed here**, and the cost is why:
    /// compiling one module is 1.4–2.0 s of CPU in a debug host (`.letibot/notes/
    /// a-built-in-layer-costs-two-seconds-per-session-open.md`), and a session that no head ever
    /// attaches to would pay it for a screen nobody draws. `ensure_the_room_pane` installs the one
    /// such layer at the door a head comes through.
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
                // `register_builtin_bytes` has already recorded the failure on the stack; this is
                // the line a person reading the daemon's log sees while it happens.
                eprintln!("  plugin built-in `{}` did not load: {why}", builtin.name);
            }
        }
    }

    /// **Install the built-in whose pane this DAEMON answers** — the room's — the first time a head
    /// comes through a door that needs it.
    ///
    /// The built-in panes are otherwise the HEAD's (`letibot_builtins::install` in the TUI's own
    /// stack): a pane is a screen, and the head owns the screen. The room's is the exception, and
    /// the reason is the whole slice — its four controls are the HOST's verbs, and the host that can
    /// perform them is this daemon, over the session's own connection to the coordinator. A head
    /// that drew it would have a pane whose keys send nothing.
    ///
    /// **Lazily, and that is a measurement, not a habit.** Compiling one built-in module costs
    /// 1.4–2.0 s of CPU in a debug host, and this layer is 218 KB — so installing it at every
    /// session open would put two seconds on every `leticode` on a box with a coordinator, for a
    /// pane most sessions never show. A session's pane is only worth a compile once a HEAD is
    /// looking at that session, so the install happens at [`PluginHost::attach`] (and at `open`,
    /// for a head that asks without having announced).
    ///
    /// Nothing happens on a box with no coordinator: `room` is `None` and there is no queue to draw.
    fn ensure_the_room_pane(&self) {
        if self.room.is_none() {
            return;
        }
        // **A layer that already answers the hook is the one that will draw it** — ours would sit
        // below it and never be called, and a project that shipped its own room pane has said
        // which one it wants.
        if self.lock().stack.layers().iter().any(|l| {
            l.manifest
                .hooks
                .iter()
                .any(|h| h.strip_prefix("pane_") == Some(ROOM_HOOK))
        }) {
            return;
        }
        let Some(builtin) = letibot_builtins::table().iter().find(|b| {
            Manifest::parse(b.manifest).is_ok_and(|m| {
                m.hooks
                    .iter()
                    .any(|h| h.strip_prefix("pane_") == Some(ROOM_HOOK))
            })
        }) else {
            return;
        };
        if let Err(why) =
            self.lock()
                .stack
                .register_builtin_bytes(builtin.name, builtin.manifest, builtin.wasm)
        {
            eprintln!("  plugin built-in `{}` did not load: {why}", builtin.name);
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    /// **Ask one hook of this session's stack**, recording the input for the gate's replay.
    ///
    /// `None` when nothing in this session exports the hook at all — a session with no layers, or
    /// one whose layers are all about panes — which is what makes a session that patches nothing
    /// cost what it cost before there were plugins: no record, no call, no allocation beyond the
    /// input nobody built.
    ///
    /// The failure path is the stack's own (`docs/plugins-plan.md` §1 principle 4): a layer that
    /// traps, runs out of fuel or answers something that is not JSON is skipped, the layer below
    /// answers, and the failure is said to the model — which is the one that can fix it.
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

    /// **One notice through the `notice` hook** (`docs/plugins-plan.md` §4.3): the sentence to
    /// record, or `None` when a layer dropped it.
    ///
    /// The kinds and which of them may be dropped are `letibot_plugin::notice`'s, not this
    /// function's: a projection over them is a second answer to *what is the model owed*, and the
    /// host is the only place that question has one. A layer that tries to drop a kind the model
    /// is owed is refused and told why, rather than silently obeyed.
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

    /// **A layer's answer the host could not use**, said once — to the model, which is the one that
    /// can fix the layer, and to the session's log, so a person watching sees why nothing changed.
    ///
    /// Not through the `notice` hook: a layer that fails must not be able to fail again about its
    /// own failure.
    fn complain(&self, hub: &Arc<Hub>, layer: &str, what: &str) {
        let said = format!("a plugin layer's answer was refused: {what}");
        {
            let mut inner = self.lock();
            if !inner.said.insert(said.clone()) {
                return;
            }
            inner.notices.push(said.clone());
        }
        hub.publish(SessionEvent::Warning {
            code: "plugin_layer".into(),
            detail: if layer.is_empty() {
                said
            } else {
                format!("{layer}: {said}")
            },
            compaction: None,
        });
    }

    /// The pane ids this session's layers export: `<manifest name>/<hook name>`.
    ///
    /// From the manifest's declared hooks rather than from the loaded module's exports, which are
    /// the same list wherever a module loaded at all (`Manifest::admit` refuses a layer whose two
    /// disagree). A layer whose module failed to load is listed with the rest: the pane exists and
    /// answers nothing, which is a screen saying so rather than a pane that is not there.
    pub fn panes(&self) -> Vec<String> {
        pane_names(&self.lock().stack)
    }

    /// **Say what discovery could not load, and publish every pane's first frame.**
    ///
    /// This is the half that makes a head's panes list possible: C4 has a head learn a pane from
    /// `SessionEvent::PluginPane`, and the view a call answers is also its title — so a session
    /// with layers has panes before anybody opens one. **No look runs here**: a dashboard nobody
    /// has opened must not read files on the session's behalf.
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

    /// When this host next has something to do: the next poll or the earliest pane refresh — or
    /// nothing, for a session with no layers at all.
    pub fn next_at(&self) -> Option<Instant> {
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

    /// **The daemon's idle pass**: hot reload if its clock is up, then every pane whose `every_s`
    /// has come round.
    ///
    /// The CHEAP half runs here — a stat per layer, which is what `poll_changes` is — and only a
    /// change starts a thread. So a session on this clock costs a few `stat`s every two seconds,
    /// and the gate, which runs `cargo`, never runs on the worker: a turn must not wait for a
    /// compiler.
    pub fn tick(self: Arc<Self>, hub: Arc<Hub>) {
        // **D17's book moves under us, and a pin that broke is news.** Before the hot-reload
        // poll, because a pin a person took in the HEAD has to be in force here at the next tick
        // — and a layer rebuilt after its pin has to stop being free in the same place, whether
        // or not the gate has caught up with the module.
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

    /// **Gate every changed layer now, synchronously** — the whole of the hot-reload tick, minus
    /// the thread.
    ///
    /// Each changed module is a [`letibot_plugins::Candidate`]; the gate replays it against a
    /// shadow of this stack and keeps the module it loaded **on the candidate**, which is what
    /// lets `Stack::apply` swap in exactly the bytes that were replayed rather than a second
    /// compile of them. A refusal leaves the stack exactly as it was.
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

    /// **A layer was pinned and its bytes moved** — said once, by name, in both registers a person
    /// reads: the head's warning stream and the model's notice.
    ///
    /// It is `plugin_untrusted` and not `plugin_refused`: nothing was refused and nothing is
    /// broken. What happened is that a layer a person said *these bytes are mine* about is not
    /// that layer any more, so every call it makes is adjudicated again until somebody pins it.
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
        hub.publish(SessionEvent::Warning {
            code: "plugin_untrusted".into(),
            detail: said,
            compaction: None,
        });
    }

    /// **A candidate was refused: the old layer stays, and somebody is told.**
    ///
    /// The model is told because it is the one that can fix the plugin, and the head is told so a
    /// person watching sees why the pane did not change (C3's *"told either way"*).
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
        hub.publish(SessionEvent::Warning {
            code: "plugin_refused".into(),
            detail: said,
            compaction: None,
        });
    }

    /// **The stack changed: bring every screen into line with it.**
    ///
    /// Three things, and they are three because a pane list can change in three ways at once: a
    /// pane that was open and is still there is asked AGAIN WITH ITS LOOK (a reload is a new
    /// version of the answer, not a reason to redraw the frame that asks for the file); a pane
    /// that is gone is published as `PluginPaneGone`; and a pane that is new is announced, which
    /// is how a head learns a title for a pane it has never heard of (C4).
    ///
    /// Called from the hot-reload gate, from `plugin load` and from `plugin disable`/`enable` —
    /// one function, because three answers to *what is on a screen now* is how they start to
    /// disagree.
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
            // **The look comes first, because that is what a refresh is**: the pane asked for the
            // file, not for a call — a second call with no new look would draw the same numbers.
            let (spec, by) = {
                let inner = self.lock();
                match inner.panes.get(&pane) {
                    Some(open) => (open.spec.clone(), open.by.clone()),
                    None => continue,
                }
            };
            let look = spec.map(|spec| {
                let caps = self.caps_of(&by);
                look::run(&spec, &self.workspace, &caps)
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

    /// The capabilities the layer that answered declared — what a look is checked against.
    ///
    /// The manifest, and not a grant: `docs/plugins-plan.md` §3 says a capability is *enforced by
    /// the host at the moment a plugin declares a look*, and this is that moment. A layer nobody
    /// can name gets nothing, which is the safe direction.
    fn caps_of(&self, layer: &str) -> Vec<Capability> {
        self.lock()
            .stack
            .layer(layer)
            .map(|l| l.manifest.capabilities.clone())
            .unwrap_or_default()
    }

    /// **The rectangle a head is drawing this pane in** — `ClientFrame::PaneOpen`'s `w` and `h`,
    /// recorded before the pane is asked so the C3 input carries it.
    ///
    /// `(0, 0)` is a head built before protocol 51, which has no size to give; it asks for
    /// [`SIZE_W`]×[`SIZE_H`] rather than for a pane zero columns wide.
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

    /// **The feeds a pane is handed, by the answering layer's own declaration** — and only those.
    ///
    /// The layer is found the way [`Stack::call`] finds it (the first in stack order that is not
    /// disabled, speaks this contract and exports the hook), so the declaration read here is the
    /// declaration of the layer whose answer will be drawn.
    ///
    /// `sessions` and `room` are built; the four other feeds the contract names are still planned,
    /// and a layer that declares one is handed **no key** — not an empty list, which would say
    /// *there are none* (see [`feed`], [`room_feed`]).
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
                    if let Some(source) = self.room.as_ref() {
                        out.insert(
                            want.name().to_string(),
                            room_feed::room(&**source, &self.session, feed::now_ms()),
                        );
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// **The feeds the layer that will answer `hook` declared**, by name — read off the stack, and
    /// only off the layer [`Stack::call`] will reach (the first in stack order that is not disabled,
    /// speaks this contract and exports the hook).
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

    /// **The acts the layer that will answer `hook` declared** — the allowlist [`View::validate_for`]
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

    /// **One pane, asked once** — and, when the answer asks for a look and this pass may run one,
    /// asked again with the look's result.
    ///
    /// The answer is published as `SessionEvent::PluginPane` whatever it is: a view that validates
    /// as the plugin wrote it, and one that does not as a single panel saying why (see this
    /// module's header).
    fn render(&self, hub: &Arc<Hub>, pane: &str, req: Req) {
        let Some((manifest, name)) = pane.split_once('/') else {
            // A pane id is `<manifest name>/<hook name>`; anything else names no hook, so there is
            // nothing to ask and nothing to say.
            return;
        };
        let hook = format!("pane_{name}");
        // **The feeds, before the lock.** `feeds_for` reads the stack to learn what the answering
        // layer declared, and the mutex is not reentrant: asking inside `render`'s own guard is a
        // deadlock, which reads as a daemon that stopped answering panes.
        let feeds = self.feeds_for(&hook);
        // **The acts the answering layer declared**, read before the lock for the same reason the
        // feeds are: `declared_actions` takes the stack's mutex and this one is not reentrant.
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
                // **A call that failed and a hook nobody answers are different screens.**
                // Failure: the layer trapped, ran out of fuel or answered something that is not
                // JSON, so the pane says why (C3's rule) rather than a head drawing a name it
                // cannot fill. Nothing at all: the layer that had it is gone, disabled, or its
                // module failed to load — the pane is gone, and one nobody opened says nothing.
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
                        hub.publish(SessionEvent::Warning {
                            code: "plugin_view_refused".into(),
                            detail: said,
                            compaction: None,
                        });
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
            // **One look per pass.** A look's result is the next call's input, and an answer that
            // asks for another is what `every_s` schedules — not what a render loops on.
            if step == 0
                && req.may_look
                && let Some(spec) = view.look.clone()
            {
                let caps = interior
                    .stack
                    .layer(&by)
                    .map(|l| l.manifest.capabilities.clone())
                    .unwrap_or_default();
                let result = look::run(&spec, &self.workspace, &caps);
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

// -------------------------------------------------------------------------------------------
// The `plugin` tool's half of this module: scaffold, build, load, stack, disable/enable, test.
//
// Every one of these asks the LIVE stack — the one `PaneDriver` draws panes from — because a
// second stack would be a second answer to *what is in force*, which is the one question a
// layered design cannot afford two of (`docs/plugins-plan.md` §2.3).
// -------------------------------------------------------------------------------------------

/// **A refusal the `plugin` tool hands a model**: the sentence saying what did not happen, and the
/// detail it is fixed from — a compiler's own words, the gate's report, the layers there are.
///
/// Two fields rather than one string, because a failed build is exactly the case: the reason is
/// *"cargo build failed"* and the detail is forty lines of `error[E0425]`, which is the part a
/// model edits. The tool's own [`letibot_tools::builtins::plugin::Refusal`] is the same shape
/// because it crosses a crate boundary; [`desk`] is the one place the two are converted.
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

    /// **`plugin build`** — the plugin's source compiled for `wasm32-unknown-unknown`, and the
    /// artifact installed as its `plugin.wasm`.
    ///
    /// It is the gate's own step 1 through the gate's own [`Builder`], so the two can never
    /// disagree about what a build is: `--offline`, `--release`, the wasm target, and the
    /// artifact copied from wherever cargo put it. The compiler's own words come back as the
    /// detail of a refusal, verbatim — a summary of an `E0425` is a model editing the wrong line.
    pub fn build(&self, name: &str) -> Result<String, Refused> {
        let dir = self.layer_dir(name)?;
        match self.builder.build(&dir) {
            Ok(log) => {
                let wasm = dir.join(letibot_plugins::WASM);
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

    /// **`plugin load`** — the shadow gate (§2.4), then a synthetic call of every pane the
    /// candidate exports, then the swap, hot.
    ///
    /// The gate's own five steps are `letibot_plugins::gate`'s and are not repeated here. What
    /// this adds is the step a NEW layer needs and a modified one does not: the gate replays the
    /// inputs a hook has already been asked, and a layer nobody has ever called has none — so
    /// without a synthetic call a first-time pane would be applied having never run once. That is
    /// exactly the measured rake of this slice (a committed `.wasm` that had never been executed
    /// and trapped on every call), so the call is made on the SHADOW, before anything on a screen
    /// changes, and a trap or a view the host would refuse keeps the live stack untouched.
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

        // **The shadow runs it, and the live stack does not.** A clone, the candidate applied to
        // the clone, every recorded input of every pane hook the candidate exports plus one
        // synthetic first frame — and each answer parsed and validated as the view the host's keys
        // must be able to drive.
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
                    "its plugin.toml declares {:?}, and the kinds are {}",
                    cand.manifest.hooks,
                    contract::KINDS.join(", ")
                ),
            ));
        }
        let mut said = Vec::new();
        for hook in &hooks {
            let mut inputs = rec.inputs(hook).to_vec();
            // **A layer nobody has asked has no recorded inputs**, so one of its own kind's
            // first frames goes too — that is what makes `load` a proof that the layer runs at all
            // rather than a check on an empty replay.
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
        Ok(format!(
            "loaded `{name}` as layer `{id}` — the shadow gate passed and it is answering now, with \
             no restart.\n\n{}\n\npanes it draws: {}",
            said.join("\n"),
            if mine.is_empty() {
                "none (a layer that answers no pane hook)".to_string()
            } else {
                mine.join(", ")
            }
        ))
    }

    /// **`plugin stack`** — what is in force, newest first.
    ///
    /// The answering layer per hook is the topmost ENABLED layer that exports it, which is what
    /// the order means; whether that layer then passes or masks is a question only a call answers,
    /// so it is `plugin test`'s and the wording here says so rather than pretending to have
    /// measured it.
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
                truncate(&layer.manifest.name, 26),
                truncate(&layer.manifest.hooks.join(","), 28),
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
            // A declaration-only layer (a `plugin.toml` and no `plugin.wasm` — the documented
            // install of a built-in's tool) has no bytes and no failure to report, so the sentence
            // says which state `sha —` above is rather than claiming every layer loaded.
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

    /// **`plugin disable` / `plugin enable`** — the bisect for *"which of my fixes broke this"*.
    ///
    /// `what` is a layer id (`s2`), a plugin's name (`plan-dashboard`) or a pane id
    /// (`plan-dashboard/dashboard`); a name that matches more than one layer is refused with the
    /// ids, never guessed at.
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

    /// **`plugin test`** — one hook called with one input, and the whole answer reported: what it
    /// answered, the layer that answered, the fuel, any trap, and whether the host would accept it.
    ///
    /// A PANE ID is asked through the whole stack, because that is the question a head asks. A
    /// LAYER id is asked with every other disk layer disabled, because that is the question a bisect
    /// asks — *what does MY layer say* — and the answer says which of the two was asked.
    pub fn test(&self, what: Option<&str>, input: Option<&Value>) -> Result<String, Refused> {
        let (hook, only, label) = {
            let inner = self.lock();
            self.resolve_hook(&inner.stack, what)?
        };
        // **The synthetic input is of the HOOK's kind**, not a pane's: a `condition` handed a C3
        // first frame would answer nothing and read as a broken layer.
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
                        let _ = alone.set_disabled(&layer.id, true);
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
                // **The acts the answering layer declared**, so a diagnostic asks the same
                // question the running host asks: a row reaching a verb the manifest never named
                // is refused here too, rather than reported as a pane that is fine.
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
                        if hook.starts_with("pane_") {
                            if let Ok(view) = validated(answer, &hook, &by, &declared) {
                                if let Some(spec) = &view.look {
                                    out.push_str(&format!(
                                        "\nit asks the host for a look: {} — a head runs it and \
                                         calls the pane again with the result, so this frame is \
                                         the one that asks.\n",
                                        serde_json::to_string(spec).unwrap_or_default()
                                    ));
                                }
                            }
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

    /// **Which hook `test` asks, and whether it is asked alone.**
    ///
    /// A pane id is the whole stack's question; a layer id (or a plugin name, or a pane id, which
    /// [`one_layer`] resolves the same way) is that layer's own.
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
                                "its plugin.toml declares {:?}; the kinds are {}",
                                layer.manifest.hooks,
                                contract::KINDS.join(", ")
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

/// C3's input, once, for a caller that has none of its own: the first frame of any pane — nothing
/// looked at, nothing pressed, no state.
fn synthetic_pane_input(feeds: &BTreeMap<String, Value>) -> Value {
    input_value(&None, &Req::fresh(), (SIZE_W, SIZE_H), feeds)
}

/// **The first frame of any hook**, for `plugin test` and for `load`'s replay of a layer nobody has
/// asked yet: the SDK's own synthetic example of that kind, which is also what `plugin_hooks`
/// prints. A layer that cannot answer this one traps on its own first frame, the cheapest defect to
/// find — and each kind's example is the plan's own.
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

/// **Would the host accept this answer for `hook`?** — the same parse the running host does, in one
/// place, so `plugin test` and `load` cannot disagree with what a session would live with.
///
/// `Ok` is the one line a person reads (what the answer WOULD do); `Err` is the refusal.
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

/// **One layer, by id, by manifest name, or by pane id** — and never a guess between two.
///
/// A name that matches two layers (a session fix and the project layer it was promoted from) is
/// refused with the ids, because *disable the plugin called x* has two answers there and a host
/// that picked one would be moving a layer nobody named.
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
/// result, or a watch — the fields the reader needs to recognise WHICH input went wrong.
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

/// A field in the stack view: long enough to recognise, short enough to line up.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let kept: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

impl PaneDriver for PluginHost {
    fn attach(&self, _session_id: &str, hub: &Arc<Hub>) {
        // **The room's pane is installed at this door**, because a head arriving is the first
        // moment the pane is worth a compile (see [`PluginHost::ensure_the_room_pane`]).
        self.ensure_the_room_pane();
        self.announce(hub);
    }

    fn open(&self, _session_id: &str, hub: &Arc<Hub>, pane: &str, size: (u32, u32)) {
        // A head that asks for a pane without having been announced one still gets it.
        self.ensure_the_room_pane();
        self.record_size(pane, size);
        self.render(hub, pane, Req::fresh());
    }

    /// **The pane is off the screen, so it comes off the refresh clock.** Nothing else is touched:
    /// the layer's view is dropped with the entry, and the next open asks for a fresh one anyway
    /// (C4's *a view is a moment*). The rectangle the pane was drawn in stays recorded, so a
    /// reopen on the same screen does not go back to the host's default for one frame.
    fn close(&self, _session_id: &str, pane: &str) {
        self.lock().panes.remove(pane);
    }

    fn action(&self, _session_id: &str, hub: &Arc<Hub>, pane: &str, item: &str, action: &str) {
        // **`look` re-runs the look now**, because that is what the key meant: a person asked for
        // the file to be read again, not for the pane to be called with its old answer.
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
                    look: spec.map(|spec| look::run(&spec, &self.workspace, &caps)),
                    event: Some((item.to_string(), action.to_string())),
                    may_look: true,
                },
            );
            return;
        }
        // **A room pane's keys are the ROOM's four verbs, and this host performs them.** The layer
        // drew the row and said which entry and which verb; what a claim or a release DOES is the
        // room's state machine, one process away, over this session's own connection — so the act
        // is taken here rather than handed back to the layer as an `event` word, which would be a
        // layer deciding what the queue does.
        //
        // The row's words were already gated: `View::validate_for` refuses a view whose `enter` or
        // whose letter names a verb the layer did not declare, so what arrives here is one of the
        // four the manifest listed.
        if let Some(room) = self.room.as_ref()
            && pane_hook(pane) == ROOM_HOOK
        {
            // **The fifth control is not a room verb.** A restart is a daemon lifetime: it goes to
            // the supervisor that starts coordinators, and the coordinator never performs it on
            // itself — the pane is fed by the instance it would kill. Its own note (`room_restarted`)
            // and its refusal (`room_restart_refused`) are the two sentences, and both are said:
            // a control whose outcome nobody printed is a control a person cannot trust.
            if action == RESTART_VERB {
                // **The act runs off this thread**, because this one is the head's own connection
                // and a launcher that overran its minute would leave the person's prompts, keys
                // and interrupts queued behind it. The outcome is the notice either way — a
                // control whose result nobody printed is a control a person cannot trust.
                match crate::room::supervise::InFlight::take() {
                    None => {
                        let _ = hub.publish(SessionEvent::Warning {
                            code: "room_restart_refused".into(),
                            detail: "the room was not restarted: a restart is already running \
                                     here — a box has one room, and two stops and two starts is \
                                     not a restart"
                                .into(),
                            compaction: None,
                        });
                    }
                    Some(running) => {
                        let room = Arc::clone(room);
                        let hub = Arc::clone(hub);
                        let _ = std::thread::Builder::new()
                            .name("room-restart".to_string())
                            .spawn(move || {
                                let _running = running;
                                // **Two literal codes**, not one variable:
                                // `crates/sessionlog/tests/warning_codes.rs` scans this tree for
                                // `Warning { code: "…" }` literals, and a code that reaches the wire
                                // through a variable is a code the guard cannot see.
                                match room.restart() {
                                    Ok(said) => {
                                        let _ = hub.publish(SessionEvent::Warning {
                                            code: "room_restarted".into(),
                                            detail: said,
                                            compaction: None,
                                        });
                                    }
                                    Err(why) => {
                                        let _ = hub.publish(SessionEvent::Warning {
                                            code: "room_restart_refused".into(),
                                            detail: format!("the room was not restarted: {why}"),
                                            compaction: None,
                                        });
                                    }
                                }
                            });
                    }
                }
            } else if let Err(why) = room.act(item, action) {
                // **A refusal is said, not swallowed.** A key that does nothing is
                // indistinguishable from a key that was not pressed, and the room's own sentence
                // names who holds the tag or what the state is.
                hub.publish(SessionEvent::Warning {
                    code: "room_act_refused".into(),
                    detail: format!("`{action}` on `{item}`: {why}"),
                    compaction: None,
                });
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

/// **The hook whose rows are the room's four controls.**
///
/// `plugins/builtin/room` declares `hooks = ["pane_room"]`, so a head opens it as `room/room`. The
/// match is on the HOOK and not on the layer's name, because what makes a key the room's verb is
/// what the pane is drawing: a project that ships its own layer over `pane_room` is drawing the
/// same room, and its rows are the same four controls.
pub const ROOM_HOOK: &str = "room";

/// **The pane's fifth control, and the one verb here that is not the room's.**
///
/// `claim`, `release`, `abandon` and `rule` are the room's own state machine; `restart` is a daemon
/// lifetime, asked of the supervisor (`crate::room::supervise`). It is declared in the layer's
/// manifest like the other four (`plugins/builtin/room/plugin.toml`), so `View::validate_for` still
/// refuses a verb nobody reviewed.
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

/// C3's input, once: the state the pane returned, the look's result, the key that was pressed, the
/// rectangle, and **the feeds the answering layer declared**. Every field is present, because a
/// plugin deserialises this into a struct and a missing key is a plugin that cannot tell *no look*
/// from *an empty one* — `feeds` is a map, and empty is a layer that declared none.
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

/// **The plugin's answer, as a view this host can drive** — or the sentence saying why not.
///
/// `validate` is the contract's own keys and shapes; `validate_for` is the layer's MANIFEST, and it
/// is the one that matters here: a row that asks the host to perform a verb the layer did not
/// declare is refused before it is drawn, rather than drawn and then refused per keypress. The
/// daemon is the host that performs those verbs, so it is the host that must hold the line — a
/// head's own check would be a second one, on a view that already crossed the wire.
fn validated(answer: &Value, pane: &str, by: &str, declared: &[String]) -> Result<View, String> {
    let view: View = serde_json::from_value(answer.clone()).map_err(|e| {
        format!("the answer is not a C3 view ({e}), while `{pane}` was resolved from `{by}`")
    })?;
    view.validate_for(declared)?;
    Ok(view)
}

/// **A pane that could not answer, as a pane that says so** — C3's rule, and the reason this
/// function exists: an empty dashboard and a broken plugin look identical on a screen.
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

/// Say each stack failure once, and queue it for the model — which is the one that can fix a
/// layer, and the reason §1 principle 4 calls the fall-through *loud*.
fn say_failures(hub: &Arc<Hub>, inner: &mut Inner, failures: &[Failure]) {
    for failure in failures {
        let said = failure.to_string();
        if !inner.said.insert(said.clone()) {
            continue;
        }
        inner.notices.push(format!(
            "a plugin layer failed and the layer below it answered: {said}"
        ));
        hub.publish(SessionEvent::Warning {
            code: "plugin_layer".into(),
            detail: said,
            compaction: None,
        });
    }
}

/// **Where a session's own layers live**: `<data dir>/sessions/<id>/plugins` — C2's session
/// scope, and the directory `plugin scaffold` writes into.
///
/// One function, because three callers have to agree about it and a disagreement is a plugin the
/// scaffold writes where nothing discovers it: [`PluginHost::for_session`] discovers this scope,
/// `plugin scaffold` writes into it, and the daemon's `Sessions::open_plugins` builds the host
/// over it. `None` only when the data dir itself cannot be resolved (no `$HOME`, no `XDG_DATA_HOME`).
pub fn session_plugins_dir(session: &str) -> Option<PathBuf> {
    crate::mergequeue::git::letibot_data_dir()
        .map(|dir| dir.join("sessions").join(session).join("plugins"))
}

/// `$XDG_CONFIG_HOME/letibot`, else `~/.config/letibot` — where the user scope's layers are.
fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|base| base.join("letibot"))
}

/// **The tools this session's layers declare**, in precedence order, one per tool name.
///
/// Read at `Harness::open`, before any module is loaded and before the daemon has a hub to build a
/// plugin host with — a declared tool is prompt bytes, and the prompt is composed first. So this
/// reads `plugin.toml`s only ([`Stack::declared_tools`] does the three disk scopes).
///
/// # Why the built-in scope is NOT read here
///
/// **A built-in is what the binary carries; it is not what this session asked for.** MEASURED
/// 2026-10-28 against this tree's own rule — `crates/harnessd/tests/wired.rs` pins the default
/// session's tool set and says *"the next addition is not a decision until it is written down here
/// too"* — appending the built-in table's declarations here seated `claude_transcript` in every
/// session, including the one opened with no seat, no project layer and no flag. A session that
/// passes no flag must get what it got before, and this function is the one place that decides it.
///
/// So a tool is seated when a layer **in force for this session** declares it — the session's own,
/// the project's, or the user's — and that declaration is the ask: it is a file a person wrote,
/// in a scope a person installs, and the stack resolves the hook through it to whatever answers
/// below. For the binary's own layers that means the built-in's `plugin.toml`, copied into
/// `<project>/.letibot/plugins/claude-transcript/`, with **no `plugin.wasm`**: the manifest is the
/// declaration the prompt is built from and the module that answers it is the one the binary
/// already carries (`PluginHost::production` installs it, and only for a tool this session
/// declared — see there). The built-in's `[[tools]]` block is therefore the text a person installs
/// and what `admit` checks the module against; it is not a tool every session is offered.
///
/// **First declaration of a name wins**, which is the stack's own rule: session over project over
/// user. A project layer that declares a built-in's tool replaces its schema and description, and
/// the hook is resolved through the same stack at call time — so the override can be a manifest
/// and a module, or a manifest alone with the built-in's module still answering below it.
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

/// What one hook's answer was, and the layer it came from — the by-line a rewrite is reported
/// with.
#[derive(Debug, Clone, PartialEq)]
pub struct Answered<T> {
    pub out: T,
    pub by: String,
}

/// **The `monitor` call a layer's `arm` becomes** — the same door the model's own calls and
/// `pr_watch` use, so the coalescing, the already-watched refusal and the TTL caps are the tool's
/// and not this function's.
///
/// `repeat` is on, matching the harness's own pull-request watch: a watch armed to notice one
/// push's checks is worth having for the next push's too, and the layer can say otherwise by
/// retiring it.
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

/// **The layer that answers a hook, as a look needs it**: its id, the name its `plugin.toml`
/// declares, the scope it came from, and what it said it may look at.
///
/// A name AND an id because the two are read by different readers: the id (`s3`, `b-github-pr`) is
/// what `plugin stack`, `set_disabled` and every message about a layer use, and the name is what a
/// person wrote in the manifest — the one a sentence to a model should carry, since `s3` means
/// nothing without the stack beside it.
pub struct Answering {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub capabilities: Vec<Capability>,
    /// **Whether this layer runs freely** — a built-in, or a layer whose bytes a person pinned
    /// (D17). An `exec` look of an unpinned one goes through the gate; of a free one, it runs.
    pub trusted: bool,
}

/// The same layer, as a sentence a model reads: `name (scope)`.
impl std::fmt::Display for Answering {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.scope)
    }
}

impl PluginHost {
    /// **The layer that answers `hook`** — the topmost ENABLED layer that exports it, which is the
    /// one the stack would ask first: a look a lower layer declared is only run when the upper one
    /// asked below, and then the upper one's capabilities are still the ones in force for the
    /// answer that came back.
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

    /// The capabilities the layer that answers `hook` declared — what a look runs under.
    pub fn capabilities_for(&self, hook: &str) -> Vec<Capability> {
        self.answering(hook)
            .map(|l| l.capabilities)
            .unwrap_or_default()
    }

    /// **The layer `id`, as the sentence a model reads** — its manifest name and its scope,
    /// `pr-watch (project)`.
    ///
    /// `s3` is a layer's identity to every mechanism (the stack view, `disable`, the audit rows),
    /// and it is nothing to a reader: three characters with no meaning unless they have
    /// `plugin stack` open beside them. So a sentence that has to tell a model *whose words these
    /// are* says the name somebody wrote, and the scope that says how far the layer reaches. A
    /// layer nobody can find (a stacked answer whose layer went away) is answered by its id, which
    /// is the honest nothing rather than an invented name.
    pub fn layer_sentence(&self, id: &str) -> String {
        let inner = self.lock();
        match inner.stack.layer(id) {
            Some(l) => format!("{} ({})", l.manifest.name, l.scope),
            None => id.to_string(),
        }
    }

    /// **A look the gate would not run**, said **once** — per layer, program and reason, because
    /// the sentence names all three and it is the dedup key.
    ///
    /// A condition polls: `every_s` is ninety seconds or more, and a refusal that repeated itself
    /// every firing would be the same warning forever, which is how a model learns to skip them.
    /// A refusal that CHANGES — the operator granted the program, the layer declared another one,
    /// the gate's own reason moved — is a different sentence and is news again. That is the rule
    /// `complain` follows for a layer's malformed answer, and the reason is the same.
    ///
    /// It reaches the session's log and the model's queue both: the model is the one that can
    /// change the layer, and the operator is the one who can grant what the look needs.
    pub fn look_refused(&self, hub: &Arc<Hub>, said: String) {
        {
            let mut inner = self.lock();
            if !inner.said.insert(said.clone()) {
                return;
            }
            inner.notices.push(said.clone());
        }
        hub.publish(SessionEvent::Warning {
            code: "plugin_look".into(),
            detail: said,
            compaction: None,
        });
    }

    /// **The last inputs this session's host asked `hook` with**, newest last — the record
    /// `docs/plugins-plan.md` §5 calls *"the last inputs of each hook per session, so a layer is
    /// written and tested against the very call or firing that went wrong"*, and what the gate's
    /// replay runs over.
    ///
    /// Public because the record is the session's, not this module's: a head drawing what a layer
    /// has been asked, `plugin test`'s replay and a test that wants to see the input a look's result
    /// came back in all read the same ring, and a second reader of it would be a second record.
    pub fn recorded(&self, hook: &str) -> Vec<Value> {
        self.lock().rec.inputs(hook).to_vec()
    }

    /// The tree a look resolves against — the session's own workspace.
    pub fn workspace(&self) -> PathBuf {
        self.workspace.clone()
    }

    /// **One result through the `on_tool_result` hook** (`docs/plugins-plan.md` §4.2): the call,
    /// how it ended, and what the model would read.
    ///
    /// `None` when nothing answers the hook (a session with no layers), or when the stack's answer
    /// is a pass or a mask. An answer that is not the contract is refused and reported, and the
    /// call's own result stands — a result the model is owed is never lost to a layer's typo.
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
    /// **One call through the `on_tool_call` hook** (`docs/plugins-plan.md` §4.1) — before the
    /// gate, so that what a layer rewrites is what the gate then adjudicates.
    ///
    /// `None` when nothing answers the hook, or when the answer is a pass or a mask. An answer that
    /// is not the contract is refused and reported, and the call runs as the model proposed it —
    /// the gate still decides, so a layer's typo cannot admit anything.
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
        let answer = resolved.answer?;
        let by = resolved.by.clone().unwrap_or_default();
        match serde_json::from_value::<ToolCallOut>(answer.clone()) {
            Ok(out) => Some(Answered { out, by }),
            Err(why) => {
                self.complain(
                    hub,
                    &by,
                    &format!(
                        "the layer `{by}` answered something that is not a call's verdict — \
                         `pass`, `arguments`, `refuse` and `answer` are the whole of it: {why}. \
                         Its answer was {answer}, so the call runs as the model proposed it"
                    ),
                );
                None
            }
        }
    }
}

/// **The runtime's `on_tool_call`, backed by one session's plugin host.**
///
/// A slot rather than the host itself, for the reason [`desk::SessionPlugins`] is: the runtime is
/// built when the HARNESS opens, and the host when the DAEMON opens the session
/// (`Sessions::open_plugins`) — the one place a hub and a pane driver exist. A session the daemon
/// does not serve (a subagent) leaves the slot empty, and its calls run exactly as they did.
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
        // **An `answer` is somebody else's words standing in for a result.** The tool never ran,
        // and the payload — the layer's own sentence — cannot say so: it reads exactly like output
        // a tool produced. So the result carries one line naming the layer in the words a person
        // wrote (`name (scope)`, not `s3`) and saying the tool did not run. A `refuse` needs none:
        // its OUTCOME already says a plugin layer refused it, and a rewrite runs and carries its
        // own note.
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

impl PluginHost {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// **Where a write's provenance goes** — D17's third arm, on the daemon's side of
/// `letibot_tools::provenance::Sink`.
///
/// It filters to what is about a layer (`Trust::note_write` does: a `plugin.toml`, a module, or a
/// file beside one) and appends to the trust book, so *who wrote these bytes* is a fact on disk.
/// A record is one line per path, rewritten by the newest write — the question a person asks is
/// *who put this here*, not *how many times*.
///
/// **The book is its own `Arc<Trust>`, not the host's.** The runtime is built when the harness
/// opens and the host when the daemon opens the session, so the two cannot share an object; they
/// share the FILE, which is the thing that matters, and the host's stack re-reads it on its poll.
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

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_plugins::{BuildError, Manifest, Module};
    use letibot_tools::runtime::GateDecision;
    use letibot_tools::schema::Access;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A module that answers from a file beside it, so a test can change what a layer says by
    /// writing to disk — which is what a hot reload is.
    struct FileModule {
        hooks: Vec<String>,
        answer: PathBuf,
    }

    impl Module for FileModule {
        fn hooks(&self) -> Vec<String> {
            self.hooks.clone()
        }

        fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
            // **Two frames, like the real example**: a pane handed a look's result answers its
            // dashboard, and one that is not answers the frame that asks for it. `second.json` is
            // that dashboard, and its absence makes a layer a single answer.
            let got_look = serde_json::from_str::<Value>(input)
                .map(|v| !v["look"].is_null())
                .unwrap_or(false);
            let second = self.answer.with_file_name("second.json");
            let path = match (got_look, second.is_file()) {
                (true, true) => second,
                _ => self.answer.clone(),
            };
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(Some(text))
        }

        fn fuel(&self) -> u64 {
            7
        }
    }

    /// The layer directory holds the answer, so the loader and the builder agree about what the
    /// module says.
    struct FileLoader;

    impl Loader for FileLoader {
        fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            let _ = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
            let dir = wasm.parent().unwrap_or(Path::new("."));
            let manifest = Manifest::read(dir)?;
            Ok(Box::new(FileModule {
                hooks: manifest.hooks.clone(),
                answer: dir.join("answer.json"),
            }))
        }
    }

    /// **The gate's step 1, without cargo**: the layer's `next.json` becomes its `plugin.wasm` and
    /// its `answer.json`, and a `FAIL` file makes the build fail. A test that ran the real
    /// `CargoBuilder` would need the wasm target and a real crate to build, which is exactly what
    /// the `Builder` seam exists to avoid.
    struct FileBuilder;

    impl Builder for FileBuilder {
        fn build(&self, dir: &Path) -> Result<String, BuildError> {
            let log = format!("built {}", dir.display());
            if dir.join("FAIL").exists() {
                return Err(BuildError {
                    why: "cargo build failed".into(),
                    log: format!("{log}\nerror: cannot find value `plan` in this scope"),
                });
            }
            let next = dir.join("next.json");
            if next.exists() {
                std::fs::copy(&next, dir.join("answer.json")).expect("an answer");
                std::fs::copy(&next, dir.join("plugin.wasm")).expect("a module");
            }
            Ok(log)
        }
    }

    static N: AtomicU64 = AtomicU64::new(0);

    fn tempdir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "letibot-plugins-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp dir");
        path
    }

    /// A project with one layer that answers `pane_dashboard` with the text of `answer.json`.
    fn project(tag: &str, answer: &str) -> (PathBuf, Arc<Hub>) {
        let root = tempdir(tag);
        let layer = root.join(".letibot/plugins/plan-dashboard");
        std::fs::create_dir_all(&layer).expect("the layer dir");
        std::fs::write(
            layer.join("plugin.toml"),
            "name = \"plan-dashboard\"\n\
             purpose = \"a project plan at a glance\"\n\
             hooks = [\"pane_dashboard\"]\n\
             capabilities = [\"read\"]\n\
             contract = 1\n",
        )
        .expect("a manifest");
        std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
        std::fs::write(layer.join("answer.json"), answer).expect("an answer");
        (root, Hub::new("s-plugins"))
    }

    fn host(root: &Path) -> PluginHost {
        PluginHost::discover(
            "s-plugins",
            root,
            None,
            None,
            Arc::new(FileLoader),
            Arc::new(FileBuilder),
        )
    }

    /// A view that validates: one panel, one key, no host key.
    const A_VIEW: &str = r#"{"title":"one","panels":[{"id":"plan","key":"t","title":"plan",
        "summary":[{"markdown":"a summary"}]}],"every_s":0}"#;

    fn views(hub: &Arc<Hub>) -> Vec<(String, Value)> {
        hub.retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::PluginPane { pane, view } => Some((pane.clone(), view.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_layers_pane_is_named_after_the_manifest_and_the_hook() {
        let (root, _hub) = project("names", A_VIEW);
        let host = host(&root);
        assert_eq!(host.panes(), vec!["plan-dashboard/dashboard".to_string()]);
    }

    #[test]
    fn announce_publishes_the_first_frame_of_every_pane_and_runs_no_look() {
        let (root, hub) = project("announce", A_VIEW);
        let host = host(&root);
        host.announce(&hub);
        let seen = views(&hub);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].0, "plan-dashboard/dashboard");
        assert_eq!(seen[0].1["title"], json!("one"));
    }

    #[test]
    fn a_read_look_outside_the_workspace_is_refused() {
        let root = tempdir("outside");
        let elsewhere = tempdir("elsewhere");
        let secret = elsewhere.join("plan.json");
        std::fs::write(&secret, "{}").expect("a file outside the workspace");
        // An ABSOLUTE path, which is the interesting case: `join` on one replaces the root, and
        // what refuses it is the containment check on the canonicalised path.
        let result = look::run(
            &LookSpec::Read {
                read: secret.display().to_string(),
            },
            &root,
            &[Capability::Read],
        );
        assert_eq!(result["exit"], json!(1), "{result}");
        assert!(
            result["stderr"]
                .as_str()
                .unwrap()
                .contains("outside the session's workspace"),
            "{result}"
        );
        // And a `..` that climbs out, which is the same check one spelling over.
        let climbing = look::run(
            &LookSpec::Read {
                read: "../plan.json".into(),
            },
            &root,
            &[Capability::Read],
        );
        assert_eq!(climbing["exit"], json!(1), "{climbing}");
    }

    #[test]
    fn a_read_look_without_the_read_capability_is_refused() {
        let root = tempdir("nocap");
        std::fs::write(root.join("plan.json"), "{}").expect("a file");
        let result = look::run(
            &LookSpec::Read {
                read: "plan.json".into(),
            },
            &root,
            &[],
        );
        assert_eq!(result["exit"], json!(1), "{result}");
        assert!(
            result["stderr"]
                .as_str()
                .unwrap()
                .contains("did not declare the `read` capability"),
            "{result}"
        );
    }

    #[test]
    fn a_read_look_inside_the_workspace_answers_the_text_and_a_missing_file_is_exit_one() {
        let root = tempdir("inside");
        std::fs::write(root.join("plan.json"), "{\"release\":{\"name\":\"v1\"}}").expect("a file");
        let result = look::run(
            &LookSpec::Read {
                read: "plan.json".into(),
            },
            &root,
            &[Capability::Read],
        );
        assert_eq!(result["exit"], json!(0), "{result}");
        assert_eq!(result["stdout"], json!("{\"release\":{\"name\":\"v1\"}}"));
        assert_eq!(result["stderr"], json!(""));

        // A file that is not there is a refusal the pane can draw, not an error the host raises.
        let missing = look::run(
            &LookSpec::Read {
                read: ".letibot/status.json".into(),
            },
            &root,
            &[Capability::Read],
        );
        assert_eq!(missing["exit"], json!(1), "{missing}");
        assert!(!missing["stderr"].as_str().unwrap().is_empty());
    }

    #[test]
    fn an_exec_look_answers_126_and_runs_nothing() {
        let root = tempdir("exec");
        let result = look::run(
            &LookSpec::Exec {
                exec: vec!["rm".into(), "-rf".into(), "/".into()],
            },
            &root,
            &[Capability::Exec("rm".into())],
        );
        assert_eq!(result["exit"], json!(126), "{result}");
        assert_eq!(result["stderr"], json!(look::EXEC_REFUSED));
    }

    #[test]
    fn a_read_path_tree_is_readable_from_a_pane_look_and_nothing_else_is() {
        // `read` alone is the workspace; `read:<path>` is one tree outside it, and the pane's look
        // runner enforces the same rule the tool's does.
        let workspace = tempdir("readpath-ws");
        let elsewhere = tempdir("readpath-tree");
        let file = elsewhere.join("a.jsonl");
        std::fs::write(&file, "{}").expect("a file in the declared tree");
        let caps = [Capability::ReadPath(elsewhere.display().to_string())];
        let allowed = look::run(
            &LookSpec::Read {
                read: file.display().to_string(),
            },
            &workspace,
            &caps,
        );
        assert_eq!(allowed["exit"], json!(0), "{allowed}");

        // A file OUTSIDE the declared tree, which exists — the check is on where a path resolves
        // to, and a path that is merely absent fails earlier and for a different reason.
        let other = tempdir("readpath-out");
        let outside = other.join("not-declared.json");
        std::fs::write(&outside, "{}").expect("a file outside the declared tree");
        let refused = look::run(
            &LookSpec::Read {
                read: outside.display().to_string(),
            },
            &workspace,
            &caps,
        );
        assert_eq!(refused["exit"], json!(1), "{refused}");
        assert!(
            refused["stderr"]
                .as_str()
                .unwrap()
                .contains("outside every tree"),
            "{refused}"
        );
    }

    // -----------------------------------------------------------------------------------------
    // The `tool_` seam
    // -----------------------------------------------------------------------------------------

    /// A module that answers one scripted answer per call, so a test can stand in the middle of a
    /// look round trip: round 0 asks for data, round 1 answers from it.
    struct Rounds {
        hooks: Vec<String>,
        answers: Vec<String>,
        at: std::sync::atomic::AtomicUsize,
    }

    impl Module for Rounds {
        fn hooks(&self) -> Vec<String> {
            self.hooks.clone()
        }

        fn call(&self, _hook: &str, _input: &str) -> Result<Option<String>, String> {
            let n = self.at.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match self.answers.get(n) {
                Some(answer) => Ok(Some(answer.clone())),
                None => Err("the layer answered more times than the test scripted".to_string()),
            }
        }

        fn fuel(&self) -> u64 {
            7
        }
    }

    /// The loader for [`Rounds`]: the answers come from `answer.json` and then `second.json`, and
    /// a `TRAP` file makes the module refuse to answer at all.
    struct RoundLoader;

    impl Loader for RoundLoader {
        fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            let _ = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
            let dir = wasm.parent().unwrap_or(Path::new("."));
            let manifest = Manifest::read(dir)?;
            let mut answers = Vec::new();
            for name in ["answer.json", "second.json"] {
                let path = dir.join(name);
                if let Ok(text) = std::fs::read_to_string(&path) {
                    answers.push(text);
                }
            }
            Ok(Box::new(Rounds {
                hooks: manifest.hooks.clone(),
                answers,
                at: std::sync::atomic::AtomicUsize::new(0),
            }))
        }
    }

    /// A project with one layer that declares a tool, and the declarations the prompt would see.
    fn tool_project(
        tag: &str,
        first: &str,
        second: &str,
        caps: &str,
    ) -> (PathBuf, Vec<DeclaredTool>) {
        let root = tempdir(tag);
        let layer = root.join(".letibot/plugins/claude-transcript");
        std::fs::create_dir_all(&layer).expect("the layer dir");
        std::fs::write(
            layer.join("plugin.toml"),
            format!(
                "name = \"claude-transcript\"\npurpose = \"read another agent's store\"\n\
                 hooks = [\"tool_probe\"]\ncapabilities = [{caps}]\ncontract = 1\n\n\
                 [[tools]]\nname = \"probe\"\ndescription = \"read what the layer says\"\n\n\
                 schema = {{ type = \"object\", properties = {{}} }}\n"
            ),
        )
        .expect("a manifest");
        std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
        std::fs::write(layer.join("answer.json"), first).expect("an answer");
        std::fs::write(layer.join("second.json"), second).expect("a second answer");
        let declared = Stack::declared_tools(None, Some(&root), None);
        (root, declared)
    }

    /// The tool, wired to a host built over `root` — the two halves `Harness::open` and
    /// `Sessions::open_plugins` put together, in the order they put them.
    ///
    /// **The layer is PINNED**, which is what D17 asks of anything placed after the binary: a
    /// layer whose bytes nobody recorded is adjudicated on every call, and the tests in this
    /// module are about the seam between a tool and its layer rather than about that ladder. The
    /// ladder's own tests are [`a_layer_nobody_pinned_is_adjudicated_on_every_call`] and
    /// [`a_pinned_layer_runs_freely_and_a_changed_one_is_refused_by_name`].
    fn tool_for(
        root: &Path,
        declared: DeclaredTool,
        hub: &Arc<Hub>,
    ) -> crate::plugins::tool::PluginTool {
        tool_for_with(root, declared, hub, true)
    }

    /// [`tool_for`], with the pin taken or not — the one argument the ladder's tests vary.
    fn tool_for_with(
        root: &Path,
        declared: DeclaredTool,
        hub: &Arc<Hub>,
        pinned: bool,
    ) -> crate::plugins::tool::PluginTool {
        let trust = letibot_plugins::Trust::in_memory();
        if pinned {
            trust
                .pin(
                    &declared.layer,
                    &root.join(".letibot/plugins").join(&declared.layer),
                )
                .expect("the layer's bytes are pinned");
        }
        let host = Arc::new(
            PluginHost::discover(
                "s-tool",
                root,
                None,
                None,
                Arc::new(RoundLoader),
                Arc::new(FileBuilder),
            )
            .with_trust(trust),
        );
        let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
            Arc::new(std::sync::Mutex::new(Some(host)));
        crate::plugins::tool::PluginTool::new(
            declared,
            "s-tool",
            root.to_path_buf(),
            slot,
            hub.clone(),
        )
    }

    /// **D17's third arm: a model's write records the sha256 of what it wrote.**
    ///
    /// The tool reports the bytes; `Provenance` is what turns that into a record in the trust
    /// book, and `Trust::note_write` is what decides the report is about a layer at all. The hash
    /// is cross-checked against the one `Trust::pin` computes from the same file, so the record
    /// and the pin cannot disagree about which bytes they are talking about.
    #[test]
    fn a_models_write_records_the_sha256_of_the_bytes_it_wrote() {
        let root = tempdir("provenance");
        let trust = letibot_plugins::Trust::open(root.join("plugin-trust.json"));
        let sink = Provenance::over(trust.clone(), "s-writer");
        let dir = root.join(".letibot/plugins/one");
        std::fs::create_dir_all(&dir).expect("the layer dir");
        let manifest = dir.join("plugin.toml");
        std::fs::write(
            &manifest,
            "name = \"one\"\npurpose = \"p\"\nhooks = [\"pane_x\"]\ncapabilities = []\ncontract = 1\n",
        )
        .expect("a manifest");
        let bytes = std::fs::read(&manifest).expect("the bytes");
        let wrote = {
            use letibot_tools::provenance::Sink as _;
            sink.wrote(&manifest, &bytes);
            // A file that is not about a layer is not provenance about one, and a book of every
            // write on the box would bury the records that matter.
            sink.wrote(&root.join("README.md"), b"# not a layer");
            trust.writes_for(&dir)
        };
        assert_eq!(
            wrote.len(),
            1,
            "one record, for the layer's own file: {wrote:?}"
        );
        assert_eq!(wrote[0].session, "s-writer");
        let pin = trust.pin("one", &dir).expect("a pin");
        assert_eq!(
            wrote[0].sha256, pin.manifest,
            "the record and the pin name the same bytes"
        );
        // And it is a fact on disk: a second book over the same file reads it back.
        let reopened = letibot_plugins::Trust::open(trust.path().unwrap());
        assert_eq!(
            reopened.writes().len(),
            1,
            "the record survives the process"
        );
    }

    /// **D17's second arm: a layer nobody pinned is adjudicated on EVERY call.**
    ///
    /// The subject of the decision is the call itself — the tool's name, its arguments and the
    /// class its layer's capabilities make it — and it is `admit_unasked`, so a layer can never
    /// put a question to a person. This test stands a decider in for the gate, because what is
    /// under test is the seam: *did this tool ask, on this call, before the layer answered?*
    #[test]
    fn a_layer_nobody_pinned_is_adjudicated_on_every_call() {
        struct Says(GateDecision, std::sync::Mutex<Vec<String>>);
        impl crate::plugins::tool::Decides for Says {
            fn admit_unasked(
                &self,
                name: &str,
                access: Access,
                _args: &Value,
            ) -> Option<GateDecision> {
                self.1.lock().unwrap().push(format!("{name}/{access:?}"));
                Some(match &self.0 {
                    GateDecision::Admit => GateDecision::Admit,
                    GateDecision::Refuse { outcome, tell } => GateDecision::Refuse {
                        outcome: outcome.clone(),
                        tell: tell.clone(),
                    },
                })
            }
        }

        let (root, declared) = tool_project(
            "tool-unpinned",
            "{\"outcome\":\"ok\",\"payload\":\"answered\"}",
            "{}",
            "",
        );
        let hub = Hub::new("s-tool");
        let tool = tool_for_with(&root, declared[0].clone(), &hub, false);

        // **Admitted by the ladder, and only then does the layer answer.**
        let admits = Says(GateDecision::Admit, std::sync::Mutex::new(Vec::new()));
        let out = tool.call(Some(&admits), &json!({}));
        assert_eq!(out.payload, "answered", "the ladder admitted it");
        assert_eq!(
            *admits.1.lock().unwrap(),
            vec!["probe/Read".to_string()],
            "the gate was asked once, about this tool"
        );
        // **Every call**, not the first: a layer whose bytes nobody pinned is decided again each
        // time it is asked for something.
        let _ = tool.call(Some(&admits), &json!({}));
        assert_eq!(admits.1.lock().unwrap().len(), 2);

        // **Refused by the ladder, and the refusal names the layer and both hashes.**
        let refuses = Says(
            GateDecision::Refuse {
                outcome: letibot_transcript::ToolOutcome::NotRun {
                    why: "it would need your approval".into(),
                },
                tell: String::new(),
            },
            std::sync::Mutex::new(Vec::new()),
        );
        let out = tool.call(Some(&refuses), &json!({}));
        match &out.outcome {
            letibot_transcript::ToolOutcome::NotRun { why } => {
                assert!(why.contains("would need your approval"), "{why}")
            }
            other => panic!("a refused layer is `not_run`, not {other:?}"),
        }
        assert!(
            out.payload.contains("`claude-transcript`"),
            "{}",
            out.payload
        );
        assert!(out.payload.contains("not pinned"), "{}", out.payload);
        assert!(out.payload.contains("sha256"), "{}", out.payload);

        // **And with no gate at all, it does not run either** — `run` is the caller that has
        // none, and a layer nobody pinned runs only where the ladder can be asked.
        let out = tool.run(&json!({}));
        assert!(matches!(
            out.outcome,
            letibot_transcript::ToolOutcome::NotRun { .. }
        ));
        assert!(out.payload.contains("no gate"), "{}", out.payload);
    }

    /// **D17's first and third arms, end to end**: a pinned layer runs freely (the gate is never
    /// asked), and a byte change after the pin un-pins it — the refusal names the layer and both
    /// hashes, and the session is told once by name.
    #[test]
    fn a_pinned_layer_runs_freely_and_a_changed_one_is_refused_by_name() {
        struct Never;
        impl crate::plugins::tool::Decides for Never {
            fn admit_unasked(&self, _: &str, _: Access, _: &Value) -> Option<GateDecision> {
                panic!("a pinned layer runs freely: the gate is never asked")
            }
        }

        let (root, declared) = tool_project(
            "tool-pinned",
            "{\"outcome\":\"ok\",\"payload\":\"answered\"}",
            "{}",
            "",
        );
        let hub = Hub::new("s-tool");
        let trust = letibot_plugins::Trust::in_memory();
        let dir = root.join(".letibot/plugins/claude-transcript");
        let pin = trust.pin("claude-transcript", &dir).expect("a pin");
        let host = Arc::new(
            PluginHost::discover(
                "s-tool",
                &root,
                None,
                None,
                Arc::new(RoundLoader),
                Arc::new(FileBuilder),
            )
            .with_trust(trust.clone()),
        );
        let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
            Arc::new(std::sync::Mutex::new(Some(host.clone())));
        let tool = crate::plugins::tool::PluginTool::new(
            declared[0].clone(),
            "s-tool",
            root.clone(),
            slot,
            hub.clone(),
        );
        let out = tool.call(Some(&Never), &json!({}));
        assert_eq!(out.payload, "answered", "a pinned layer runs freely");

        // **The bytes move** — the module is rebuilt, which is the shape the rule exists for.
        // The stack picks the rebuild up the way it does in the daemon: the poll sees it, the gate
        // applies it, and the pin is decided again against what was applied.
        std::fs::write(dir.join("plugin.wasm"), b"a module, rebuilt").expect("a rebuild");
        let pending = host.lock().stack.poll_changes();
        assert_eq!(pending.len(), 1, "the rebuild is a candidate");
        host.lock()
            .stack
            .apply(pending.into_iter().next().unwrap())
            .expect("the rebuilt layer applies");
        let news = host.lock().stack.refresh_trust();
        assert_eq!(news.len(), 1, "the pin broke: {news:?}");
        assert_eq!(news[0].name, "claude-transcript");
        // The layer is adjudicated again, so the decider that panics for a free layer is replaced
        // by one that refuses — which is what an unpinned layer's call meets.
        struct Refuses;
        impl crate::plugins::tool::Decides for Refuses {
            fn admit_unasked(&self, _: &str, _: Access, _: &Value) -> Option<GateDecision> {
                Some(GateDecision::Refuse {
                    outcome: letibot_transcript::ToolOutcome::NotRun {
                        why: "it would need your approval".into(),
                    },
                    tell: String::new(),
                })
            }
        }
        let out = tool.call(Some(&Refuses), &json!({}));
        assert!(
            matches!(out.outcome, letibot_transcript::ToolOutcome::NotRun { .. }),
            "{:?}",
            out.outcome
        );
        assert!(
            out.payload.contains(&pin.module),
            "the pinned hash: {}",
            out.payload
        );
        assert!(
            out.payload.contains("not pinned any more"),
            "{}",
            out.payload
        );
    }

    #[test]
    fn a_declared_tool_is_seated_and_answers_a_result_through_the_stack() {
        let (root, declared) = tool_project(
            "tool-ok",
            "{\"outcome\":\"ok\",\"payload\":\"the layer's own words\"}",
            "{}",
            "",
        );
        let hub = Hub::new("s-tool");
        let tool = tool_for(&root, declared[0].clone(), &hub);
        // What the model sees is the manifest's declaration, and nothing else.
        let schema = letibot_tools::runtime::Tool::schema(&tool);
        assert_eq!(schema.name, "probe");
        assert_eq!(schema.description, "read what the layer says");
        assert_eq!(schema.parameters["type"], json!("object"));
        let out = tool.run(&json!({}));
        assert_eq!(
            out.outcome,
            letibot_tools::runtime::Invocation::ok("").outcome
        );
        assert_eq!(out.payload, "the layer's own words");
        // And the result says who answered it, so a model can tell a layer's words from a tool's.
        assert!(
            out.notes.iter().any(|n| n.contains("answered by plugin")),
            "{:?}",
            out.notes
        );
    }

    #[test]
    fn a_tool_asks_for_a_look_and_the_host_runs_it_against_the_declared_tree() {
        let elsewhere = tempdir("tool-tree");
        let secret = elsewhere.join("a.jsonl");
        std::fs::write(&secret, "{\"line\":1}\n{\"line\":2}\n").expect("a file");
        let first = json!({
            "looks": [{"read": secret.display().to_string(), "lines": 1}],
            "state": {"step": 1},
        })
        .to_string();
        // The second answer echoes what it was handed, so the test can read the look's result.
        let second = "{\"outcome\":\"ok\",\"payload\":\"<ECHO>\"}";
        let caps = format!("\"read:{}\"", elsewhere.display());
        let (root, declared) = tool_project("tool-look", &first, second, &caps);
        let hub = Hub::new("s-tool");
        let tool = tool_for(&root, declared[0].clone(), &hub);
        // The echo is not a thing a guest can do, so the assertion is on the recorder: the stack
        // kept the input of the second call, which carries the look's result.
        let out = tool.run(&json!({}));
        assert_eq!(
            out.outcome,
            letibot_tools::runtime::Invocation::ok("").outcome
        );
        assert!(out.payload.contains("<ECHO>"), "{}", out.payload);
    }

    #[test]
    fn a_look_outside_the_declared_tree_is_refused_by_the_host_not_by_the_layer() {
        let (root, declared) = tool_project(
            "tool-outside",
            "{\"looks\": [{\"read\": \"/etc/hosts\"}]}",
            "{\"outcome\":\"ok\",\"payload\":\"second\"}",
            "",
        );
        // No capability at all: the layer may look at nothing, and the host is what says so.
        let looked = crate::plugins::tool::run_look(
            &json!({"read": "/etc/hosts"}),
            &declared[0].capabilities,
            &root,
        );
        assert_eq!(looked["exit"], json!(1), "{looked}");
        assert!(
            looked["error"]
                .as_str()
                .unwrap()
                .contains("declares no `read` capability"),
            "{looked}"
        );
        // And a tree it DID declare is readable, which is the other half of the same rule.
        let tree = tempdir("tool-declared");
        let file = tree.join("a.jsonl");
        std::fs::write(&file, "one\ntwo\n").expect("a file");
        let caps = [Capability::ReadPath(tree.display().to_string())];
        let ok = crate::plugins::tool::run_look(
            &json!({"read": file.display().to_string(), "from": 2, "lines": 1}),
            &caps,
            &root,
        );
        assert_eq!(ok["exit"], json!(0), "{ok}");
        assert_eq!(ok["lines"][0]["text"], json!("two"));
        assert_eq!(ok["lines"][0]["n"], json!(2));
    }

    #[test]
    fn a_symlink_inside_the_declared_tree_is_refused_for_where_it_points() {
        // `~/.claude/projects` really holds one of these (`memory` → a folder under `~/Projects`),
        // and the check is on the RESOLVED path for exactly this reason.
        let tree = tempdir("tool-symlink");
        let outside = tempdir("tool-symlink-out");
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "not for this layer").expect("a file outside");
        let link = tree.join("memory");
        std::os::unix::fs::symlink(&outside, &link).expect("a symlink");
        let caps = [Capability::ReadPath(tree.display().to_string())];
        let looked = crate::plugins::tool::run_look(
            &json!({"read": link.join("secret.txt").display().to_string()}),
            &caps,
            &tempdir("tool-symlink-ws"),
        );
        assert_eq!(looked["exit"], json!(1), "{looked}");
        assert!(
            looked["error"]
                .as_str()
                .unwrap()
                .contains("outside every tree"),
            "{looked}"
        );
    }

    #[test]
    fn a_layer_that_traps_or_answers_nonsense_is_a_failed_tool_not_a_dead_session() {
        // Nonsense: an object with neither `outcome` nor `looks`.
        let (root, declared) = tool_project("tool-nonsense", "{\"hello\":\"world\"}", "{}", "");
        let hub = Hub::new("s-tool");
        let out = tool_for(&root, declared[0].clone(), &hub).run(&json!({}));
        assert!(matches!(
            out.outcome,
            letibot_transcript::ToolOutcome::Failed { .. }
        ));
        assert!(
            out.payload.contains("neither `outcome` nor `looks`"),
            "{}",
            out.payload
        );

        // An outcome nobody defined.
        let (root, declared) = tool_project(
            "tool-outcome",
            "{\"outcome\":\"probably\",\"payload\":\"x\"}",
            "{}",
            "",
        );
        let hub = Hub::new("s-tool");
        let out = tool_for(&root, declared[0].clone(), &hub).run(&json!({}));
        assert!(out.payload.contains("the outcomes are"), "{}", out.payload);
    }

    #[test]
    fn a_session_with_no_plugin_host_says_so_rather_than_failing_the_turn() {
        let (root, declared) = tool_project("tool-nohost", "{}", "{}", "");
        let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let tool = crate::plugins::tool::PluginTool::new(
            declared[0].clone(),
            "s-sub",
            root,
            slot,
            Hub::new("s-sub"),
        );
        let out = tool.run(&json!({}));
        match &out.outcome {
            letibot_transcript::ToolOutcome::NotRun { why } => {
                assert!(why.contains("no plugin host"), "{why}");
            }
            other => panic!("a session with no host is `not_run`, not {other:?}"),
        }
        // And the detail names what would fix it, which is the sentence a model acts on.
        assert!(out.payload.contains("parent's runner"), "{}", out.payload);
    }

    #[test]
    fn the_builtin_transcript_layer_is_on_the_daemons_stack_and_answers_its_hook() {
        // **The whole seam, with the real module**: the project installs the binary's own
        // `plugin.toml` (that declaration is the ask — `crates/harnessd/src/plugins.rs`'s
        // `declared_tools` seats nothing the session did not declare), `crates/builtins` compiled
        // `plugins/builtin/claude-transcript` for `wasm32-unknown-unknown` and the host puts it at
        // the bottom of this session's stack for that reason, rano loads it here, `admit` compares
        // its exports with the manifest's `[[tools]]`, and the hook runs. If any of those five is
        // wrong this test is red before anything reaches a model.
        let workspace = tempdir("builtin-tool");
        let builtin = letibot_builtins::table()
            .iter()
            .find(|b| b.name == "claude-transcript")
            .expect("the binary carries the transcript layer");
        let layer = workspace.join(".letibot/plugins/claude-transcript");
        std::fs::create_dir_all(&layer).expect("the layer dir");
        // The manifest and nothing else: the module is the one the binary already carries, which is
        // the whole point of a built-in layer.
        std::fs::write(layer.join("plugin.toml"), builtin.manifest).expect("the manifest");
        let host = PluginHost::production("s-tool", &workspace);
        // **And the documented install is CLEAN.** The project wrote a `plugin.toml` and no
        // `plugin.wasm` — that is the install (`docs/plugins-plan.md` §4.8), because the module
        // that answers is the one the binary carries — so discovery must record nothing against
        // this stack and the session must be told nothing. MEASURED before the fix: two failures
        // (`plugin.wasm: No such file or directory`, twice) went out as `plugin_layer` warnings on
        // every session in the project, for a correct install, on the code that means *this layer
        // is exploding*. `stack.failures()` and the notices are what this test now looks at.
        {
            let inner = host.lock();
            assert!(
                inner.stack.failures().is_empty(),
                "a manifest with no module is a declaration, not a failure: {:?}",
                inner.stack.failures()
            );
        }
        // It is a tool and not a pane: nothing of the transcript layer's is announced to a head.
        // **The room's pane is not here either**, and that is the measurement: this host has no
        // room feed, and the room's layer is installed lazily at the door a head comes through
        // (`ensure_the_room_pane`) — a module costs 1.4–2.0 s to compile and a session nobody looks
        // at must not pay it.
        assert!(host.panes().is_empty(), "{:?}", host.panes());
        let hub = Hub::new("s-tool");
        host.announce(&hub);
        assert!(
            !hub.retained().iter().any(
                |e| matches!(&e.event, SessionEvent::Warning { code, .. } if code == "plugin_layer")
            ),
            "a correct install says nothing on the session's log: {:?}",
            hub.retained()
        );
        assert!(
            host.take_notices().is_empty(),
            "and nothing is queued for the model"
        );
        let asked = host.ask(
            &hub,
            "tool_claude_transcript",
            &json!({
                "v": 1,
                "name": "claude_transcript",
                "args": {"what": "sessions"},
                "feeds": {"home": "/nonexistent-home", "workspace": "/nonexistent-project", "now_ms": 0},
                "looks": [],
                "state": null,
            }),
        );
        let resolved = asked.expect("the built-in answers `tool_claude_transcript`");
        assert!(
            resolved.failures.is_empty(),
            "a built-in must not fail its own hook: {:?}",
            resolved.failures
        );
        let answer = resolved.answer.expect("an answer");
        // Round zero asks the host for a listing — the guest does no I/O, so the only correct
        // first answer is a look, and that is the contract working end to end.
        assert_eq!(
            answer["looks"][0]["list"],
            json!("/nonexistent-home/.claude/projects/-nonexistent-project"),
            "{answer}"
        );
    }

    /// **A session that declared no tool from a layer gets no built-in layer at all** — the other
    /// half of the test above, and the half that costs CPU.
    ///
    /// MEASURED 2026-10-28: compiling this binary's tool layer is 1.97 s in a debug host, and the
    /// first cut of §4.8 installed it for EVERY session — so every open paid it, `letibot-bridge`'s
    /// `no_spin` window was entirely compile, and `letibot-harnessd`'s idle-exit test found its
    /// 800 ms window already gone when `Sessions::open_first` returned. A session that seats no
    /// layer tool cannot call the layer: the `PluginTool` that would is what a declaration seats.
    ///
    /// The assertion is `ask` answering `None` — *nothing in this stack exports the hook* — which
    /// is the same door `Stack::call` goes through, so it is a fact about what is installed rather
    /// than a claim about it.
    #[test]
    fn a_session_that_declared_no_tool_installs_no_builtin_layer() {
        let workspace = tempdir("builtin-none");
        let host = PluginHost::production("s-nobody", &workspace);
        assert!(
            host.ask(
                &Hub::new("s-nobody"),
                "tool_claude_transcript",
                &json!({"v": 1, "args": {}, "looks": [], "state": null}),
            )
            .is_none(),
            "nothing asked for the tool, so no layer of the binary's is on this stack"
        );
    }

    #[test]
    fn a_grep_look_answers_line_numbers_byte_offsets_and_bounded_context() {
        let root = tempdir("grep");
        let file = root.join("t.jsonl");
        std::fs::write(&file, "one\ntwo\nTHREE\nfour\nfive\nsix\n").expect("a file");
        let caps = [Capability::ReadPath(root.display().to_string())];
        let looked = crate::plugins::tool::run_look(
            &json!({"grep": file.display().to_string(), "match": "three", "context": 1, "limit": 5}),
            &caps,
            &root,
        );
        assert_eq!(looked["exit"], json!(0), "{looked}");
        let hit = &looked["matches"][0];
        assert_eq!(hit["line"], json!(3));
        // Byte 8 is where the third line starts: `one\n` + `two\n` is eight bytes.
        assert_eq!(hit["byte"], json!(8));
        assert_eq!(hit["text"], json!("THREE"));
        assert_eq!(hit["before"][0]["text"], json!("two"));
        assert_eq!(hit["after"][0]["text"], json!("four"));
        // The count of what was read is what makes a miss distinguishable from a failure.
        assert_eq!(looked["scanned"], json!(6));
    }

    #[test]
    fn a_list_look_answers_names_sizes_and_mtimes_and_a_file_is_not_a_directory() {
        let root = tempdir("list");
        std::fs::write(root.join("a.jsonl"), "0123456789").expect("a file");
        std::fs::create_dir_all(root.join("subagents")).expect("a directory");
        let caps = [Capability::ReadPath(root.display().to_string())];
        let looked = crate::plugins::tool::run_look(
            &json!({"list": root.display().to_string()}),
            &caps,
            &root,
        );
        assert_eq!(looked["exit"], json!(0), "{looked}");
        let names: Vec<&str> = looked["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["name"].as_str())
            .collect();
        assert_eq!(names, vec!["a.jsonl", "subagents"]);
        let file = &looked["entries"][0];
        assert_eq!(file["size"], json!(10));
        assert_eq!(file["dir"], json!(false));
        assert!(file["mtime"].as_u64().unwrap() > 0);

        let not_a_dir = crate::plugins::tool::run_look(
            &json!({"list": root.join("a.jsonl").display().to_string()}),
            &caps,
            &root,
        );
        assert_eq!(not_a_dir["exit"], json!(1), "{not_a_dir}");
    }

    #[test]
    fn a_view_the_host_cannot_validate_becomes_one_panel_saying_why() {
        // `q` is one of the host's own keys (C3), so `View::validate` refuses this view.
        let bad = r#"{"title":"bad","panels":[{"id":"plan","key":"q","title":"plan"}]}"#;
        let (root, hub) = project("refused", bad);
        let host = host(&root);
        host.announce(&hub);
        let seen = views(&hub);
        assert_eq!(seen.len(), 1, "{seen:?}");
        let view: View = serde_json::from_value(seen[0].1.clone()).expect("a view");
        view.validate().expect("the host's own answer validates");
        let unavailable = view.panels[0]
            .unavailable
            .as_ref()
            .expect("one panel, and it is unavailable");
        assert!(unavailable.why.contains('q'), "{}", unavailable.why);
        assert_eq!(
            unavailable.fix,
            "the layer p1 answered something the host refused"
        );
    }

    #[test]
    fn an_answer_that_is_not_a_view_is_the_same_unavailable_panel() {
        let (root, hub) = project("notaview", "this is not JSON at all");
        let host = host(&root);
        host.announce(&hub);
        let seen = views(&hub);
        assert_eq!(seen.len(), 1, "{seen:?}");
        let view: View = serde_json::from_value(seen[0].1.clone()).expect("a view");
        assert!(view.panels[0].unavailable.is_some());
    }

    #[test]
    fn a_pane_that_asks_for_a_look_is_asked_again_with_its_result() {
        let (root, hub) = project("looks", A_VIEW);
        std::fs::create_dir_all(root.join(".letibot")).expect("the status dir");
        std::fs::write(
            root.join(".letibot/status.json"),
            "{\"release\":{\"name\":\"v0.2.0\"}}",
        )
        .expect("a status file");
        let layer = root.join(".letibot/plugins/plan-dashboard");
        // The two frames the real example answers: the first asks for the file and draws nothing
        // (`every_s` 0), the second is the dashboard and asks to be refreshed every 30 s.
        std::fs::write(
            layer.join("answer.json"),
            r#"{"title":"plan","panels":[{"id":"plan","key":"p","title":"plan"}],
                "look":{"read":".letibot/status.json"},"every_s":0}"#,
        )
        .expect("the frame that asks");
        std::fs::write(
            layer.join("second.json"),
            r#"{"title":"plan · v0.2.0","panels":[{"id":"tiers","key":"t","title":"tiers"}],
                "look":{"read":".letibot/status.json"},"every_s":30}"#,
        )
        .expect("the dashboard");
        let host = host(&root);
        host.render(&hub, "plan-dashboard/dashboard", Req::fresh());
        let seen = views(&hub);
        assert_eq!(
            seen.len(),
            2,
            "the frame that asks and the one after the look: {seen:?}"
        );
        assert_eq!(seen[0].1["title"], json!("plan"));
        assert_eq!(seen[1].1["title"], json!("plan · v0.2.0"));
        // **And the pane is now on the refresh clock**, from the answer that asked for 30 s.
        let inner = host.lock();
        let open = inner
            .panes
            .get("plan-dashboard/dashboard")
            .expect("the pane is open");
        assert_eq!(open.every_s, 30);
        assert!(open.next_at.is_some());
    }

    /// **A refused candidate leaves the old view alone; a passing one changes it** — the two
    /// halves of §2.3, through the same `Stack::apply` the daemon uses.
    #[test]
    fn a_refused_candidate_keeps_the_old_view_and_a_passing_one_changes_it() {
        let (root, hub) = project("gate", A_VIEW);
        let host = host(&root);
        let layer = root.join(".letibot/plugins/plan-dashboard");
        host.render(&hub, "plan-dashboard/dashboard", Req::fresh());
        assert_eq!(views(&hub).last().unwrap().1["title"], json!("one"));

        // A candidate whose build fails: `FAIL` stops the gate at step 1, and the module changed
        // so the poll has something to hand it.
        std::fs::write(layer.join("FAIL"), b"").expect("a failing build");
        std::fs::write(
            layer.join("next.json"),
            r#"{"title":"two","panels":[{"id":"plan","key":"t","title":"plan"}]}"#,
        )
        .expect("a next answer");
        std::fs::write(layer.join("plugin.wasm"), b"a changed module").expect("a module");
        assert_eq!(host.gate_now(&hub), 0, "nothing was applied");
        assert_eq!(
            views(&hub).last().unwrap().1["title"],
            json!("one"),
            "the version that answers is the one before the refusal"
        );
        let noticed = host.take_notices();
        assert!(
            noticed
                .iter()
                .any(|n| n.contains("was refused by the shadow gate") && n.contains("p1")),
            "{noticed:?}"
        );

        // And now a candidate that passes: it says something else, and the open pane redraws.
        std::fs::remove_file(layer.join("FAIL")).expect("the build works again");
        std::fs::write(layer.join("plugin.wasm"), b"a third module").expect("a module");
        assert_eq!(host.gate_now(&hub), 1, "the candidate was applied");
        assert_eq!(
            views(&hub).last().unwrap().1["title"],
            json!("two"),
            "the open pane was rendered again from the new layer"
        );
    }

    /// The recorder is what the gate replays, so a pane call has to land in it — and the pane has
    /// to see the `state` it returned last time (§1, principle 5).
    #[test]
    fn a_pane_hands_its_state_back_and_the_input_is_recorded() {
        let answer =
            r#"{"title":"one","state":{"n":2},"panels":[{"id":"plan","key":"t","title":"plan"}]}"#;
        let (root, hub) = project("state", answer);
        let host = host(&root);
        host.render(&hub, "plan-dashboard/dashboard", Req::fresh());
        host.render(&hub, "plan-dashboard/dashboard", Req::fresh());
        let inner = host.lock();
        let inputs = inner.rec.inputs("pane_dashboard");
        assert_eq!(inputs.len(), 2);
        assert_eq!(
            inputs[0]["state"],
            Value::Null,
            "the first call knows nothing"
        );
        assert_eq!(inputs[1]["state"], json!({"n": 2}), "the second is told");
        // **The contract version is the SDK's own number**, not a copy: it said `1` while the
        // contract had grown to 2, so a layer that read `v` to decide whether it would be handed a
        // feed was told *no* by a host that was about to hand it one.
        assert_eq!(inputs[1]["v"], json!(letibot_plugin::pane::V));
        assert_eq!(inputs[1]["size"], json!({"w": SIZE_W, "h": SIZE_H}));
        assert!(inner.panes.contains_key("plan-dashboard/dashboard"));
    }

    /// A stop's action reaches the pane — read off the input the host recorded, because that input
    /// IS what the plugin was handed.
    #[test]
    fn a_pane_is_handed_the_event_that_was_pressed() {
        let (root, hub) = project("event", A_VIEW);
        let host = host(&root);
        host.render(
            &hub,
            "plan-dashboard/dashboard",
            Req {
                look: None,
                event: Some(("tier1/jstests".to_string(), "expand".to_string())),
                may_look: false,
            },
        );
        let last = views(&hub).last().cloned().expect("a view");
        assert_eq!(last.1["title"], json!("one"));
        let inner = host.lock();
        let input = inner
            .rec
            .inputs("pane_dashboard")
            .last()
            .expect("the call was recorded");
        assert_eq!(input["event"]["item"], json!("tier1/jstests"));
        assert_eq!(input["event"]["action"], json!("expand"));
    }

    /// **A hook no layer answers is silence, not a blank view.** The two screens are different:
    /// `PluginPaneGone` closes a pane that was open, and nothing at all is a pane a head has never
    /// heard of.
    #[test]
    fn a_pane_nobody_answers_publishes_nothing() {
        let (root, hub) = project("gone", A_VIEW);
        let host = host(&root);
        host.render(&hub, "plan-dashboard/never", Req::fresh());
        assert!(
            views(&hub).is_empty(),
            "a hook nobody exports is not a pane with a view"
        );
    }

    /// **A module that answers its own input's `size`** — the only place a test can read, from
    /// outside, that the rectangle a head sent reached the plugin. The title is C3's one field
    /// that is free text, so the plugin reports the rectangle there and the assertion is the
    /// answer the head would draw.
    struct SizeModule;

    impl Module for SizeModule {
        fn hooks(&self) -> Vec<String> {
            vec!["pane_dashboard".to_string()]
        }

        fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
            let input: Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
            Ok(Some(
                json!({
                    "title": format!("{}\u{d7}{}", input["size"]["w"], input["size"]["h"]),
                    "panels": [{"id": "plan", "key": "t", "title": "plan"}],
                })
                .to_string(),
            ))
        }

        fn fuel(&self) -> u64 {
            7
        }
    }

    struct SizeLoader;

    impl Loader for SizeLoader {
        fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            Ok(Box::new(SizeModule))
        }
    }

    /// **The rectangle the head is drawing the pane in reaches the plugin** — C4's `PaneOpen`
    /// `w`/`h`, which exist because the daemon has no screen: a dashboard laid out for 120×40 in a
    /// 70-column pane wraps every label for a terminal nobody is looking at.
    #[test]
    fn the_rectangle_a_head_sends_is_the_one_the_pane_is_asked_at() {
        let (root, hub) = project("size", A_VIEW);
        let host = PluginHost::discover(
            "s-plugins",
            &root,
            None,
            None,
            Arc::new(SizeLoader),
            Arc::new(FileBuilder),
        );
        host.open("s-plugins", &hub, "plan-dashboard/dashboard", (77, 21));
        assert_eq!(
            views(&hub).last().map(|(_, v)| v["title"].clone()),
            Some(json!("77\u{d7}21")),
            "the plugin was handed the head's rectangle"
        );
        // **A resize is the same frame again**, with the new rectangle — one way to say *draw it
        // at this size*, whether it is the first time or the fifth.
        host.open("s-plugins", &hub, "plan-dashboard/dashboard", (100, 30));
        assert_eq!(
            views(&hub).last().map(|(_, v)| v["title"].clone()),
            Some(json!("100\u{d7}30"))
        );
        // **And a head built before protocol 51 sends neither** — `0` is *the head did not say*,
        // which asks for the host's default rather than for a pane zero columns wide.
        host.open("s-plugins", &hub, "plan-dashboard/dashboard", (0, 0));
        assert_eq!(
            views(&hub).last().map(|(_, v)| v["title"].clone()),
            Some(json!(format!("{SIZE_W}\u{d7}{SIZE_H}")))
        );
    }

    /// **A pane the head closed comes off the refresh clock** — C4's `PaneClose`. Without it the
    /// host believes somebody is looking at a closed dashboard for the rest of the session, and
    /// asks it again every `every_s`: a wasm call and a look per interval, for a screen nobody has.
    #[test]
    fn a_closed_pane_is_not_refreshed_again() {
        let (root, hub) = project("close", A_VIEW);
        let layer = root.join(".letibot/plugins/plan-dashboard");
        // On the clock: `every_s` 5 is C3's floor.
        std::fs::write(
            layer.join("answer.json"),
            r#"{"title":"one","panels":[{"id":"plan","key":"t","title":"plan"}],
                "every_s":5}"#,
        )
        .expect("an answer on the clock");
        let host = Arc::new(host(&root));
        let pane = "plan-dashboard/dashboard";
        host.open("s-plugins", &hub, pane, (80, 24));
        assert!(host.lock().panes.contains_key(pane), "the pane is open");
        // **Due now**, rather than waiting five seconds for it.
        host.lock().panes.get_mut(pane).expect("open").next_at =
            Some(Instant::now() - Duration::from_secs(1));
        let before = views(&hub).len();
        host.clone().tick(hub.clone());
        assert_eq!(
            views(&hub).len(),
            before + 1,
            "an open pane whose clock is up is asked again"
        );

        host.close("s-plugins", pane);
        assert!(
            !host.lock().panes.contains_key(pane),
            "a closed pane is not held"
        );
        let after = views(&hub).len();
        host.clone().tick(hub.clone());
        assert_eq!(
            views(&hub).len(),
            after,
            "and a closed pane is not asked again: {}",
            views(&hub).len()
        );
        // Closing twice, or a pane this session never had, is quiet rather than an error.
        host.close("s-plugins", pane);
        host.close("s-plugins", "plan-dashboard/never");
        assert_eq!(views(&hub).len(), after);
    }

    // ---------------------------------------------------------------------------------------
    // The hook fixture: a layer that answers any hook from a table
    //
    // The pane fixture above answers one hook from one file, which is enough for a pane — its
    // name never varies. A hook KIND is resolved by name, and every question these tests ask is
    // about names: which layer answers `notice_monitor`, what the layer below says when the top
    // one traps, what happens to a hook nobody exports. So the answers live in a table keyed by
    // (layer directory, hook), and a layer can answer one kind and pass the next.
    // ---------------------------------------------------------------------------------------

    /// What one hook of one layer says — one answer per call, the last one repeating, so a layer
    /// can ask below on its first call and answer on the second. `None` is a trap.
    type Script = Vec<Option<Value>>;

    #[derive(Clone, Default)]
    struct Table(Arc<Mutex<BTreeMap<String, BTreeMap<String, Script>>>>);

    /// The id a `&str` hook name gets in the table.
    fn key(hook: &str) -> String {
        hook.to_string()
    }

    impl Table {
        fn new() -> Table {
            Table::default()
        }

        /// Write one layer: its manifest, a module, and its answers.
        fn layer(self: &Table, root: &Path, name: &str, hooks: &[(&str, Script)]) -> PathBuf {
            self.layer_with_caps(root, name, &[], hooks)
        }

        /// The same layer with **capabilities declared** — the only thing §3's enforcement and the
        /// gate behind it can be asked about, since a look is checked against this list.
        fn layer_with_caps(
            self: &Table,
            root: &Path,
            name: &str,
            caps: &[&str],
            hooks: &[(&str, Script)],
        ) -> PathBuf {
            let dir = root.join(".letibot/plugins").join(name);
            std::fs::create_dir_all(&dir).expect("the layer dir");
            let declared = hooks
                .iter()
                .map(|(h, _)| format!("\"{h}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let capabilities = caps
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            std::fs::write(
                dir.join("plugin.toml"),
                format!(
                    "name = \"{name}\"\npurpose = \"one hook, answered from a table\"\n\
                     hooks = [{declared}]\ncapabilities = [{capabilities}]\ncontract = 1\n"
                ),
            )
            .expect("a manifest");
            std::fs::write(dir.join("plugin.wasm"), b"a module").expect("a module");
            let mut table = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let entry = table.entry(name.to_string()).or_default();
            for (hook, script) in hooks {
                entry.insert(key(hook), script.clone());
            }
            dir
        }
    }

    struct TableModule {
        hooks: Vec<String>,
        answers: BTreeMap<String, Script>,
        calls: Mutex<BTreeMap<String, usize>>,
    }

    impl Module for TableModule {
        fn hooks(&self) -> Vec<String> {
            self.hooks.clone()
        }

        fn call(&self, hook: &str, _input: &str) -> Result<Option<String>, String> {
            let Some(script) = self.answers.get(hook) else {
                return Ok(None);
            };
            let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
            let at = calls.entry(hook.to_string()).or_insert(0);
            let answer = script.get(*at).or_else(|| script.last()).cloned();
            *at += 1;
            match answer {
                None => Ok(None),
                Some(None) => Err("trapped: error while executing at wasm backtrace".to_string()),
                Some(Some(value)) => Ok(Some(value.to_string())),
            }
        }

        fn fuel(&self) -> u64 {
            11
        }
    }

    struct TableLoader {
        table: Table,
    }

    impl Loader for TableLoader {
        fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            let dir = wasm
                .parent()
                .ok_or_else(|| format!("{}: no directory", wasm.display()))?;
            let name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let answers = self
                .table
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&name)
                .cloned()
                .ok_or_else(|| format!("the table has no answers for `{name}`"))?;
            Ok(Box::new(TableModule {
                hooks: answers.keys().cloned().collect(),
                answers,
                calls: Mutex::new(BTreeMap::new()),
            }))
        }
    }

    /// A host over a table's layers, with no builder use in these tests (nothing is loaded
    /// through the gate here).
    fn table_host(root: &Path, table: &Table) -> PluginHost {
        PluginHost::discover(
            "s-plugins",
            root,
            None,
            None,
            Arc::new(TableLoader {
                table: table.clone(),
            }),
            Arc::new(FileBuilder),
        )
    }

    /// A one-answer script.
    fn says(answer: Value) -> Script {
        vec![Some(answer)]
    }

    /// **A layer rewords a notice, and the model reads its sentence.**
    #[test]
    fn a_layer_rewords_a_notice() {
        let root = tempdir("notice-reword");
        let table = Table::new();
        table.layer(
            &root,
            "coderabbit-wording",
            &[(
                "notice_monitor",
                says(
                    json!({"text": "[monitor] CodeRabbit reviewed o/r#24: 3 comments — read them"}),
                ),
            )],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let said = host.notice(&hub, "monitor", "[monitor] `pr-24` fired: …", json!({}));
        assert_eq!(
            said.as_deref(),
            Some("[monitor] CodeRabbit reviewed o/r#24: 3 comments — read them")
        );
        // And the input it was handed is the host's own shape, with the kind on it — the same
        // document `plugin_hooks` prints.
        let seen = host.lock().rec.inputs("notice_monitor").to_vec();
        assert_eq!(seen.len(), 1, "the input is recorded for the gate's replay");
        assert_eq!(seen[0]["kind"], json!("monitor"));
        assert_eq!(seen[0]["v"], json!(letibot_plugin::V));
    }

    /// **A mask switches the hook off from that layer down**, and the harness's sentence is what
    /// the model reads — which is the difference between a notice nobody patched and a notice a
    /// layer silenced.
    #[test]
    fn a_masked_hook_leaves_the_harnesss_own_sentence() {
        let root = tempdir("notice-mask");
        let table = Table::new();
        table.layer(
            &root,
            "a-below",
            &[("notice_monitor", says(json!({"text": "the layer below"})))],
        );
        table.layer(
            &root,
            "z-above",
            &[("notice_monitor", says(json!({"mask": true})))],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        assert_eq!(
            host.notice(&hub, "monitor", "the harness's own", json!({})),
            Some("the harness's own".to_string()),
            "a mask answers nothing and asks nothing below it"
        );
    }

    /// **A layer asks below and the lower layer really is called** — the witness is the lower
    /// layer's trap arriving in this call's failures while the top layer still answers.
    #[test]
    fn a_layer_asks_below_and_the_lower_layer_is_called() {
        let root = tempdir("notice-below");
        let table = Table::new();
        table.layer(&root, "a-below", &[("notice_monitor", vec![None])]);
        table.layer(
            &root,
            "z-above",
            &[(
                "notice_monitor",
                vec![
                    Some(json!({"below": {"v": 1, "kind": "monitor", "text": "lower"}})),
                    Some(json!({"text": "the top layer, having seen below"})),
                ],
            )],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let resolved = host
            .ask(
                &hub,
                "notice_monitor",
                &json!({"v": 1, "kind": "monitor", "text": "x"}),
            )
            .expect("a layer answers");
        assert_eq!(
            resolved.answer,
            Some(json!({"text": "the top layer, having seen below"}))
        );
        assert_eq!(resolved.by.as_deref(), Some("p2"), "the top layer answers");
        assert!(
            resolved.failures.iter().any(|f| f.why.contains("trapped")),
            "the layer below was asked, and it trapped: {:?}",
            resolved.failures
        );
    }

    /// **A disabled layer is not asked at all**, and a hook nobody answers is not recorded: the
    /// bisect has to be visible in the recorder too, or a replay would run against inputs from a
    /// layer that was off.
    #[test]
    fn a_disabled_layer_is_not_asked() {
        let root = tempdir("notice-disabled");
        let table = Table::new();
        table.layer(
            &root,
            "fix",
            &[("notice_monitor", says(json!({"text": "patched"})))],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let id = host.lock().stack.layers()[0].id.clone();
        host.lock().stack.set_disabled(&id, true).expect("disable");
        assert_eq!(
            host.notice(&hub, "monitor", "the harness's own", json!({})),
            Some("the harness's own".to_string())
        );
        assert!(
            host.ask(&hub, "notice_monitor", &json!({})).is_none(),
            "with the only layer off, nothing exports the hook"
        );
        assert_eq!(host.lock().rec.inputs("notice_monitor").len(), 0);
    }

    /// **A layer that traps falls to the one below, and the model is told which failed** — the
    /// loud half of §1 principle 4.
    #[test]
    fn a_trapping_layer_falls_down_and_is_reported() {
        let root = tempdir("notice-trap");
        let table = Table::new();
        table.layer(
            &root,
            "a-working",
            &[(
                "notice_monitor",
                says(json!({"text": "the layer below answered"})),
            )],
        );
        table.layer(&root, "z-broken", &[("notice_monitor", vec![None])]);
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        assert_eq!(
            host.notice(&hub, "monitor", "the harness's own", json!({})),
            Some("the layer below answered".to_string())
        );
        let warned = hub.retained().iter().any(
            |e| matches!(&e.event, SessionEvent::Warning { code, .. } if code == "plugin_layer"),
        );
        assert!(warned, "the failing layer is said on the session's log");
        assert!(
            host.take_notices()
                .iter()
                .any(|n| n.contains("the layer below it answered")),
            "and queued for the model, which is the one that can fix it"
        );
    }

    /// **A session with no layers says the harness's sentence, and pays nothing for it** — the
    /// promise that a session that patches nothing behaves as it did before there were plugins.
    #[test]
    fn a_session_with_no_layers_is_unchanged() {
        let root = tempdir("notice-none");
        let table = Table::new();
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        assert_eq!(
            host.notice(&hub, "monitor", "the harness's own", json!({})),
            Some("the harness's own".to_string())
        );
        assert!(
            host.ask(&hub, "notice_monitor", &json!({})).is_none(),
            "nothing was asked, so nothing was recorded"
        );
        assert!(host.lock().rec.inputs("notice_monitor").is_empty());
        assert!(hub.retained().is_empty(), "and nothing was published");
    }

    /// **A firing may be dropped and a settlement may not** (§4.3): the kinds a project never
    /// wants are the firing and the nudge, and a layer that tries to silence a fact the model is
    /// owed is refused and told why.
    #[test]
    fn a_dropped_firing_is_silent_and_a_dropped_settlement_is_refused() {
        let root = tempdir("notice-drop");
        let table = Table::new();
        table.layer(
            &root,
            "quiet",
            &[
                ("notice_monitor", says(json!({"drop": true}))),
                ("notice_job", says(json!({"drop": true}))),
            ],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        assert_eq!(
            host.notice(&hub, "monitor", "a firing", json!({})),
            None,
            "a project may switch a firing off"
        );
        assert_eq!(
            host.notice(&hub, "job", "a settlement", json!({})),
            Some("a settlement".to_string()),
            "a settlement is a fact the model is owed"
        );
        assert!(
            host.take_notices()
                .iter()
                .any(|n| n.contains("may be reworded but not dropped")),
            "and the layer is told why it did nothing"
        );
    }

    /// **An answer that is not the contract is refused, not obeyed** — a layer that says
    /// `{"text": 5}` must not silence a notice, and the model is told its layer is wrong.
    #[test]
    fn an_answer_that_is_not_a_notice_is_refused() {
        let root = tempdir("notice-wrong-shape");
        let table = Table::new();
        table.layer(
            &root,
            "wrong",
            &[("notice_monitor", says(json!({"text": 5})))],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        assert_eq!(
            host.notice(&hub, "monitor", "the harness's own", json!({})),
            Some("the harness's own".to_string())
        );
        assert!(
            host.take_notices()
                .iter()
                .any(|n| n.contains("is not a notice")),
            "the refusal names the shape"
        );
    }

    // ---------------------------------------------------------------------------------------
    // `on_tool_result` (§4.2): the result, before the model reads it
    // ---------------------------------------------------------------------------------------

    /// The call every result test here answers: the plan's own example, a pull request opened.
    fn a_call() -> letibot_plugin::Call {
        letibot_plugin::Call::new("bash", json!({"command": "gh pr create --fill"}))
    }

    /// **A layer adds a sentence to a result** — the hook's whole first use: the call printed a
    /// pull request URL and the model is told what is watching it now.
    #[test]
    fn a_layer_appends_to_a_result() {
        let root = tempdir("result-append");
        let table = Table::new();
        table.layer(
            &root,
            "watch-pr",
            &[(
                "on_tool_result",
                says(json!({"append": "\n[harness] watching o/r#24 for its checks"})),
            )],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let answered = host
            .on_tool_result(&hub, &a_call(), "ok", "https://github.com/o/r/pull/24\n")
            .expect("a layer answers");
        assert_eq!(
            answered.out.said(),
            letibot_plugin::tool_result::Said::Appended(
                "\n[harness] watching o/r#24 for its checks"
            )
        );
        assert_eq!(answered.by, "p1");
        // And the input is the host's own shape: the call as an object, the outcome as a word,
        // the payload the model would have read.
        let seen = host.lock().rec.inputs("on_tool_result").to_vec();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["call"]["name"], json!("bash"));
        assert_eq!(seen[0]["outcome"], json!("ok"));
        assert_eq!(
            seen[0]["payload"],
            json!("https://github.com/o/r/pull/24\n")
        );
        assert_eq!(seen[0]["v"], json!(letibot_plugin::V));
    }

    /// **A layer replaces a result**, and a suffix alone is a different answer.
    #[test]
    fn a_layer_replaces_a_result() {
        let root = tempdir("result-replace");
        let table = Table::new();
        table.layer(
            &root,
            "shorten",
            &[("on_tool_result", says(json!({"payload": "opened 24"})))],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let answered = host
            .on_tool_result(&hub, &a_call(), "ok", "a very long transcript")
            .expect("a layer answers");
        assert_eq!(
            answered.out.said(),
            letibot_plugin::tool_result::Said::Text("opened 24".into())
        );
    }

    /// **A mask leaves the result exactly as the call produced it** — the hook switched off from
    /// that layer down, which is what a mask means.
    #[test]
    fn a_masked_result_hook_leaves_the_call_own_words() {
        let root = tempdir("result-mask");
        let table = Table::new();
        table.layer(
            &root,
            "a-below",
            &[("on_tool_result", says(json!({"append": "the layer below"})))],
        );
        table.layer(
            &root,
            "z-above",
            &[("on_tool_result", says(json!({"mask": true})))],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        assert!(
            host.on_tool_result(&hub, &a_call(), "ok", "x").is_none(),
            "a mask answers nothing and asks nothing below it"
        );
    }

    /// **A layer asks below and edits what it is told** — the shape §2.1 describes for the
    /// dotenv-style fix, over a result rather than a notice.
    #[test]
    fn a_result_layer_asks_below_and_answers_for_itself() {
        let root = tempdir("result-below");
        let table = Table::new();
        table.layer(&root, "a-below", &[("on_tool_result", vec![None])]);
        table.layer(
            &root,
            "z-above",
            &[(
                "on_tool_result",
                vec![
                    Some(json!({"below": {"v": 1, "outcome": "ok", "payload": "x"}})),
                    Some(json!({"append": " (edited)"})),
                ],
            )],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let resolved = host
            .ask(
                &hub,
                "on_tool_result",
                &json!({"v": 1, "outcome": "ok", "payload": "x"}),
            )
            .expect("a layer answers");
        assert_eq!(resolved.by.as_deref(), Some("p2"), "the top layer answers");
        assert!(
            resolved.failures.iter().any(|f| f.why.contains("trapped")),
            "the layer below was asked: {:?}",
            resolved.failures
        );
    }

    /// **A disabled layer is not asked**, and a hook nobody answers is not recorded.
    #[test]
    fn a_disabled_result_layer_is_not_asked() {
        let root = tempdir("result-disabled");
        let table = Table::new();
        table.layer(
            &root,
            "fix",
            &[("on_tool_result", says(json!({"append": "patched"})))],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let id = host.lock().stack.layers()[0].id.clone();
        host.lock().stack.set_disabled(&id, true).expect("disable");
        assert!(host.on_tool_result(&hub, &a_call(), "ok", "x").is_none());
        assert_eq!(host.lock().rec.inputs("on_tool_result").len(), 0);
    }

    /// **A trapping layer falls to the one below, and the model is told** — a result the model is
    /// owed is never lost to a layer's trap.
    #[test]
    fn a_trapping_result_layer_falls_down_and_is_reported() {
        let root = tempdir("result-trap");
        let table = Table::new();
        table.layer(
            &root,
            "a-working",
            &[("on_tool_result", says(json!({"append": " from below"})))],
        );
        table.layer(&root, "z-broken", &[("on_tool_result", vec![None])]);
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let answered = host
            .on_tool_result(&hub, &a_call(), "ok", "x")
            .expect("the layer below answers");
        assert_eq!(
            answered.out.said(),
            letibot_plugin::tool_result::Said::Appended(" from below")
        );
        assert!(
            host.take_notices()
                .iter()
                .any(|n| n.contains("the layer below it answered")),
            "and the failure is said to the model"
        );
    }

    /// **A session with no layers hands the result back untouched, and records nothing.**
    #[test]
    fn a_session_with_no_layers_rewrites_no_result() {
        let root = tempdir("result-none");
        let table = Table::new();
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        assert!(host.on_tool_result(&hub, &a_call(), "ok", "x").is_none());
        assert!(host.lock().rec.inputs("on_tool_result").is_empty());
    }

    /// **An `arm` becomes the `monitor` call the model would have typed** — one door for a watch,
    /// so the coalescing and the refusals are the tool's and not a second implementation's here.
    #[test]
    fn an_arm_is_a_monitor_call() {
        let arm = letibot_plugin::tool_result::Arm::new("coderabbit", json!({"pr": "o/r#24"}));
        let call = arm_call(&arm);
        assert_eq!(call.name, "monitor");
        let args: Value = serde_json::from_str(&call.arguments).expect("an object");
        assert_eq!(args["name"], json!("coderabbit"));
        assert_eq!(args["plugin"], json!("coderabbit"));
        assert_eq!(args["args"], json!({"pr": "o/r#24"}));
        assert_eq!(
            args["repeat"],
            json!(true),
            "a watch repeats, as `pr_watch`'s does"
        );
    }

    // ---------------------------------------------------------------------------------------
    // `on_tool_call` (§4.1): the call, before the gate
    // ---------------------------------------------------------------------------------------

    fn standing() -> letibot_tools::runtime::Standing {
        letibot_tools::runtime::Standing {
            session: "s-plugins".into(),
            mode: "allow-edits".into(),
        }
    }

    /// **A layer rewrites a call's arguments**, and the answer says what it wants.
    #[test]
    fn a_layer_rewrites_a_call() {
        let root = tempdir("call-rewrite");
        let table = Table::new();
        table.layer(
            &root,
            "one-watch",
            &[(
                "on_tool_call",
                says(json!({"arguments": {"name": "pr-24", "until": "checks"}})),
            )],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let asked = letibot_plugin::Call::new("monitor", json!({"name": "pr-24-coderabbit"}));
        let answered = host
            .on_tool_call(&hub, &asked, &standing())
            .expect("a layer answers");
        assert!(matches!(
            answered.out.verdict(),
            letibot_plugin::tool_call::Verdict::Rewrite(_)
        ));
        assert_eq!(answered.by, "p1");
        // The input carries the session's own facts — the mode is the gate's word for how much
        // this session may do unasked, and a layer is allowed to be narrower than *always*.
        let seen = host.lock().rec.inputs("on_tool_call").to_vec();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["call"]["name"], json!("monitor"));
        assert_eq!(seen[0]["session"]["mode"], json!("allow-edits"));
        assert_eq!(seen[0]["session"]["session"], json!("s-plugins"));
        assert!(seen[0]["session"]["workspace"].is_string());
        assert_eq!(seen[0]["v"], json!(letibot_plugin::V));
    }

    /// **A layer refuses a call, and a layer answers one** — the two ways of not running it.
    #[test]
    fn a_layer_can_refuse_or_answer_a_call() {
        let root = tempdir("call-refuse");
        let table = Table::new();
        table.layer(
            &root,
            "one-watch",
            &[(
                "on_tool_call",
                says(json!({"refuse": "already watched as pr-24; use that"})),
            )],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let asked = letibot_plugin::Call::new("monitor", json!({}));
        let answered = host
            .on_tool_call(&hub, &asked, &standing())
            .expect("answers");
        assert_eq!(
            answered.out.verdict(),
            letibot_plugin::tool_call::Verdict::Refuse("already watched as pr-24; use that")
        );

        let root = tempdir("call-answer");
        let table = Table::new();
        table.layer(
            &root,
            "knows-it",
            &[(
                "on_tool_call",
                says(json!({"answer": "pr-24 is watched for its checks"})),
            )],
        );
        let host = table_host(&root, &table);
        let answered = host
            .on_tool_call(&hub, &asked, &standing())
            .expect("answers");
        assert_eq!(
            answered.out.verdict(),
            letibot_plugin::tool_call::Verdict::Answer("pr-24 is watched for its checks")
        );
    }

    /// **An `answer` verdict reaches the model with one line saying who answered it, in the
    /// layer's own name and scope** — `docs/plugins-plan.md` §4.1's `answer`, whose payload is a
    /// layer's sentence and would otherwise read exactly like a tool's output.
    ///
    /// The `Answered` id (`s3`, `p1`) is what the stack and the audit rows use, and it is what a
    /// model must NOT be handed: this is the one place the two are composed, so a change to how a
    /// layer is named moves here and nowhere else.
    #[test]
    fn an_answered_call_is_attributed_by_name_and_scope() {
        let root = tempdir("call-answer-note");
        let table = Table::new();
        table.layer(
            &root,
            "knows-it",
            &[(
                "on_tool_call",
                says(json!({"answer": "pr-24 is watched for its checks"})),
            )],
        );
        let host = Arc::new(table_host(&root, &table));
        let hub = Hub::new("s-plugins");
        let hooks = CallHooks::new(Arc::new(Mutex::new(Some(host.clone()))), hub.clone());
        let asked = letibot_plugin::Call::new("monitor", json!({}));
        let patched = letibot_tools::runtime::CallHook::before(
            &hooks,
            &asked,
            &letibot_tools::runtime::Standing::default(),
        )
        .expect("answers");
        assert_eq!(
            patched.note.as_deref(),
            Some("answered by plugin knows-it (project); the tool did not run"),
            "the note must name the layer a person wrote, not `{}`",
            patched.by
        );

        // **A refusal carries none**, because its outcome already says a layer refused it — and
        // a pass carries none either, because nothing was said.
        let refused = tempdir("call-refuse-note");
        let table = Table::new();
        table.layer(
            &refused,
            "one-watch",
            &[("on_tool_call", says(json!({"refuse": "already watched"})))],
        );
        let host = Arc::new(table_host(&refused, &table));
        let hooks = CallHooks::new(Arc::new(Mutex::new(Some(host))), hub);
        let patched = letibot_tools::runtime::CallHook::before(
            &hooks,
            &asked,
            &letibot_tools::runtime::Standing::default(),
        )
        .expect("answers");
        assert_eq!(patched.note, None);
    }

    /// **A mask lets the call through untouched** — the hook switched off from that layer down,
    /// which for this kind means the model's call is the one that runs.
    #[test]
    fn a_masked_call_hook_lets_the_call_through() {
        let root = tempdir("call-mask");
        let table = Table::new();
        table.layer(
            &root,
            "a-below",
            &[("on_tool_call", says(json!({"refuse": "the layer below"})))],
        );
        table.layer(
            &root,
            "z-above",
            &[("on_tool_call", says(json!({"mask": true})))],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let asked = letibot_plugin::Call::new("monitor", json!({}));
        assert!(host.on_tool_call(&hub, &asked, &standing()).is_none());
    }

    /// **A layer asks below before deciding** — the shape a fix uses when it wants to *narrow* what
    /// a lower layer already said rather than replace it.
    #[test]
    fn a_call_layer_asks_below_and_answers_for_itself() {
        let root = tempdir("call-below");
        let table = Table::new();
        table.layer(&root, "a-below", &[("on_tool_call", vec![None])]);
        table.layer(
            &root,
            "z-above",
            &[(
                "on_tool_call",
                vec![
                    Some(json!({"below": {"v": 1, "call": {"name": "monitor", "arguments": {}}}})),
                    Some(json!({"refuse": "no"})),
                ],
            )],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let asked = letibot_plugin::Call::new("monitor", json!({}));
        let answered = host
            .on_tool_call(&hub, &asked, &standing())
            .expect("answers");
        assert_eq!(answered.by, "p2", "the top layer answers");
        assert_eq!(
            answered.out.verdict(),
            letibot_plugin::tool_call::Verdict::Refuse("no")
        );
    }

    /// **A disabled layer is not asked**, and a hook nobody answers is not recorded.
    #[test]
    fn a_disabled_call_layer_is_not_asked() {
        let root = tempdir("call-disabled");
        let table = Table::new();
        table.layer(
            &root,
            "fix",
            &[("on_tool_call", says(json!({"refuse": "patched"})))],
        );
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let id = host.lock().stack.layers()[0].id.clone();
        host.lock().stack.set_disabled(&id, true).expect("disable");
        let asked = letibot_plugin::Call::new("monitor", json!({}));
        assert!(host.on_tool_call(&hub, &asked, &standing()).is_none());
        assert_eq!(host.lock().rec.inputs("on_tool_call").len(), 0);
    }

    /// **A trapping layer falls to the one below, and the model is told** — a call is never left
    /// undecided because a layer broke.
    #[test]
    fn a_trapping_call_layer_falls_down_and_is_reported() {
        let root = tempdir("call-trap");
        let table = Table::new();
        table.layer(
            &root,
            "a-working",
            &[("on_tool_call", says(json!({"refuse": "from below"})))],
        );
        table.layer(&root, "z-broken", &[("on_tool_call", vec![None])]);
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let asked = letibot_plugin::Call::new("monitor", json!({}));
        let answered = host
            .on_tool_call(&hub, &asked, &standing())
            .expect("answers");
        assert_eq!(
            answered.out.verdict(),
            letibot_plugin::tool_call::Verdict::Refuse("from below")
        );
        assert!(
            host.take_notices()
                .iter()
                .any(|n| n.contains("the layer below it answered")),
            "and the failure is said to the model"
        );
    }

    /// **A session with no layers shows nothing to a layer and records nothing.**
    #[test]
    fn a_session_with_no_layers_passes_every_call_through() {
        let root = tempdir("call-none");
        let table = Table::new();
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let asked = letibot_plugin::Call::new("bash", json!({"command": "ls"}));
        assert!(host.on_tool_call(&hub, &asked, &standing()).is_none());
        assert!(host.lock().rec.inputs("on_tool_call").is_empty());
        assert!(hub.retained().is_empty());
    }

    // ---------------------------------------------------------------------------------------
    // `condition` (§4.4): a monitor whose policy is a layer
    // ---------------------------------------------------------------------------------------

    /// The daemon's half, over this test's host — the real look runner and the real stack.
    ///
    /// No gate: these tests are about the condition's shape (the interval, the state, the look
    /// results), and an `exec` look here is refused for want of one. The tests that DO adjudicate
    /// one pass their own [`Gate`](conditions::Unasked); see `an_exec_look_goes_through_the_gate`.
    fn conditions(host: &Arc<PluginHost>) -> Arc<conditions::HostConditions> {
        conditions_with(host, None)
    }

    fn conditions_with(
        host: &Arc<PluginHost>,
        gate: Option<Arc<dyn conditions::Unasked>>,
    ) -> Arc<conditions::HostConditions> {
        conditions::HostConditions::new(
            Arc::new(Mutex::new(Some(host.clone()))),
            Hub::new("s-plugins"),
            gate,
        )
    }

    /// **A layer's condition arms, and its answer is what the watch follows** — the arming call,
    /// the interval, the cap, and the state, through the seam the daemon fills.
    #[test]
    fn a_layers_condition_arms_with_its_own_interval_and_cap() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("condition-arm");
        let table = Table::new();
        table.layer(
            &root,
            "coderabbit",
            &[(
                "condition_coderabbit",
                says(json!({
                    "look": {"read": "status.json"},
                    "every_s": 90,
                    "max_ttl_s": 604800,
                    "state": {"armed": true}
                })),
            )],
        );
        std::fs::write(root.join("status.json"), "{\"reviews\":3}").expect("a status file");
        let host = Arc::new(table_host(&root, &table));
        let armed = conditions(&host)
            .arm("coderabbit", json!({"pr": "o/r#24"}))
            .expect("the layer arms it");
        assert_eq!(
            armed.condition.max_ttl(),
            Some(Duration::from_secs(604800)),
            "the plugin's own cap, inside the tree's week"
        );
        assert!(armed.condition.describe().contains("coderabbit"));
        // The read look ran under the layer's `read` capability and is now ready to hand over —
        // which is the half that proves the seam reaches the workspace.
        assert_eq!(
            armed.condition.met(),
            None,
            "a file look has no firing of its own"
        );
        let seen = host.lock().rec.inputs("condition_coderabbit").to_vec();
        assert!(!seen.is_empty(), "the arming call was recorded");
        assert_eq!(seen[0]["args"], json!({"pr": "o/r#24"}));
        assert!(seen[0]["look"].is_null(), "arming asks with no look");
    }

    /// **A watch nobody answers is refused by name**, and so is one whose layer is off — the two
    /// are the same sentence to a model, and it names the hook.
    #[test]
    fn a_condition_nobody_answers_is_refused() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("condition-none");
        let table = Table::new();
        let host = Arc::new(table_host(&root, &table));
        let why = match conditions(&host).arm("coderabbit", json!({})) {
            Ok(_) => panic!("nothing answers it"),
            Err(why) => why,
        };
        assert!(
            why.why.contains("no layer answers `condition_coderabbit`"),
            "{}",
            why.why
        );

        // And a layer that IS there but switched off answers nothing either.
        let root = tempdir("condition-disabled");
        let table = Table::new();
        table.layer(
            &root,
            "coderabbit",
            &[("condition_coderabbit", says(json!({"every_s": 30})))],
        );
        let host = Arc::new(table_host(&root, &table));
        let id = host.lock().stack.layers()[0].id.clone();
        host.lock().stack.set_disabled(&id, true).expect("disable");
        assert!(conditions(&host).arm("coderabbit", json!({})).is_err());
    }

    /// **A condition layer that traps falls to the one below**, which is how a session fixes a
    /// broken watch without deleting it.
    #[test]
    fn a_trapping_condition_layer_falls_down() {
        use letibot_tools::exec::monitor::Condition as _;
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("condition-trap");
        let table = Table::new();
        table.layer(
            &root,
            "a-working",
            &[("condition_coderabbit", says(json!({"every_s": 30})))],
        );
        table.layer(&root, "z-broken", &[("condition_coderabbit", vec![None])]);
        let host = Arc::new(table_host(&root, &table));
        let armed = conditions(&host)
            .arm("coderabbit", json!({}))
            .expect("the layer below arms it");
        assert!(armed.condition.describe().contains("coderabbit"));
        assert!(
            host.take_notices()
                .iter()
                .any(|n| n.contains("the layer below it answered")),
            "and the trap is said to the model"
        );
    }

    /// **An `exec` look the layer did not declare runs nothing, and says which capability it
    /// wanted** — the enforcement point §3 names, exercised through a real armed watch.
    #[test]
    fn an_exec_look_without_the_capability_is_refused() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("condition-exec");
        let table = Table::new();
        table.layer(
            &root,
            "shell",
            &[(
                "condition_shell",
                vec![
                    Some(json!({"look": {"exec": ["true"]}, "every_s": 5})),
                    Some(json!({"fired": "the look came back"})),
                ],
            )],
        );
        let host = Arc::new(table_host(&root, &table));
        // **The gate is never reached.** The capability is checked first, because a program the
        // layer never declared is not a call anybody adjudicates — so this probe must stay empty.
        let probe = Arc::new(ProbeGate::admitting());
        let armed = conditions_with(&host, Some(probe.clone()))
            .arm("shell", json!({}))
            .expect("armed");
        // The first tick hands the refusal over, and the layer answers it.
        assert_eq!(
            armed.condition.met(),
            Some("the look came back".to_string())
        );
        let seen = host.lock().rec.inputs("condition_shell").to_vec();
        let handed = seen
            .iter()
            .find(|i| !i["look"].is_null())
            .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
        assert_eq!(
            handed["look"]["exit"],
            json!(126),
            "a program the layer did not declare is not run: {handed}"
        );
        assert!(
            handed["look"]["stderr"]
                .as_str()
                .is_some_and(|s| s.contains("exec:true")),
            "and the refusal names the capability: {handed}"
        );
        assert!(
            probe.seen().is_empty(),
            "the gate was asked about an undeclared program: {:?}",
            probe.seen()
        );
    }

    // ---------------------------------------------------------------------------------------
    // An `exec` look goes through the adjudicator (§1 principle 2), without asking
    // ---------------------------------------------------------------------------------------

    /// **A gate a test answers with, and what it was asked.** The seam's own fixture: one
    /// decision, and every command line the look offered it, so a test can say both what the gate
    /// would do and whether it was consulted at all.
    struct ProbeGate {
        answer: fn() -> GateDecision,
        seen: Mutex<Vec<String>>,
    }

    impl ProbeGate {
        fn admitting() -> ProbeGate {
            ProbeGate {
                answer: || GateDecision::Admit,
                seen: Mutex::new(Vec::new()),
            }
        }

        fn refusing(answer: fn() -> GateDecision) -> Arc<ProbeGate> {
            Arc::new(ProbeGate {
                answer,
                seen: Mutex::new(Vec::new()),
            })
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    impl conditions::Unasked for ProbeGate {
        fn admit_unasked(&self, call: &letibot_tools::GateCall<'_>) -> Option<GateDecision> {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).push(
                call.args["command"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
            Some((self.answer)())
        }
    }

    /// The warnings one session's log holds under a code.
    fn warned(hub: &Arc<Hub>, code: &str) -> Vec<String> {
        hub.retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::Warning {
                    code: c, detail, ..
                } if c == code => Some(detail.clone()),
                _ => None,
            })
            .collect()
    }

    /// A layer whose one look is `exec:<program>`, and what it answers once the look is in.
    fn shell_layer(root: &Path, program: &str, argv: Value) -> Table {
        let table = Table::new();
        table.layer_with_caps(
            root,
            "shell",
            &[&format!("exec:{program}")],
            &[(
                "condition_shell",
                vec![
                    Some(json!({"look": {"exec": argv}, "every_s": 5})),
                    Some(json!({"fired": "the look came back"})),
                ],
            )],
        );
        table
    }

    /// **Run the watch until it fires, with a deadline** — a look that is a real child is polled
    /// on the next tick, so a test that called `met()` once would be racing the process.
    fn until_fired(condition: &Arc<dyn letibot_tools::exec::monitor::Condition>) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(said) = condition.met() {
                return said;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the look never came back"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// **A built-in's `exec` look runs FREELY** — D17's first arm, which is the operator's own
    /// sentence: *"Only the builtins run freely"*, `exec` included. The layer is the binary's own
    /// bytes and nobody placed it at run time, so there is nobody to ask and nothing to pin.
    ///
    /// The gate here refuses everything it is shown, so this test fails in two ways without the
    /// arm: the probe would have been asked (it must not be), and the look's own exit would have
    /// been a refusal rather than the program's.
    #[test]
    fn a_builtins_exec_look_runs_without_the_gate() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("look-builtin");
        // No disk layer at all: the ONLY layer is the built-in below.
        let table = Table::new();
        let host = Arc::new(table_host(&root, &table));
        let answers: BTreeMap<String, Script> = [(
            key("condition_shell"),
            vec![
                Some(json!({"look": {"exec": ["true"]}, "every_s": 5})),
                Some(json!({"fired": "the look came back"})),
            ],
        )]
        .into_iter()
        .collect();
        let manifest = Manifest::parse(
            "name = \"shell\"\npurpose = \"p\"\nhooks = [\"condition_shell\"]\n\
             capabilities = [\"exec:true\"]\ncontract = 1\n",
        )
        .expect("a manifest");
        host.lock().stack.register_builtin(
            letibot_plugins::Layer {
                id: String::new(),
                scope: letibot_plugins::Scope::BuiltIn,
                dir: std::path::PathBuf::new(),
                manifest,
                sha256: String::new(),
                manifest_sha256: String::new(),
                disabled: false,
                trusted: false,
                pin: None,
            },
            Box::new(TableModule {
                hooks: answers.keys().cloned().collect(),
                answers,
                calls: Mutex::new(BTreeMap::new()),
            }),
        );
        let probe = ProbeGate::refusing(|| {
            GateDecision::refuse_and_tell(
                letibot_transcript::ToolOutcome::NotRun {
                    why: "nothing a built-in runs should have reached this gate".into(),
                },
                "",
            )
        });
        let armed = conditions_with(&host, Some(probe.clone()))
            .arm("shell", json!({}))
            .expect("armed");
        assert_eq!(until_fired(&armed.condition), "the look came back");
        assert!(
            probe.seen().is_empty(),
            "a built-in runs freely: the gate was asked {:?}",
            probe.seen()
        );
        let seen = host.lock().rec.inputs("condition_shell").to_vec();
        let handed = seen
            .iter()
            .find(|i| !i["look"].is_null())
            .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
        assert_eq!(handed["look"]["exit"], json!(0), "the program really ran");
    }

    /// **The look belongs to the layer whose ANSWER asked for it, not to the one that declares the
    /// hook** — the reviewer's hole, closed.
    ///
    /// A PINNED layer sits on top and passes the hook; the answer therefore comes from an UNPINNED
    /// layer below it, and the `exec` look in that answer is the unpinned layer's. Judged by the
    /// topmost declarer (what this used to do) the look ran unadjudicated, because the top layer
    /// was free — so an unpinned layer could run a program through a pinned layer's good name.
    /// Judged by the answering layer it is adjudicated, and this gate refuses everything.
    #[test]
    fn an_exec_look_is_judged_by_the_layer_whose_answer_asked_for_it() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("look-answerer");
        let table = Table::new();
        // `b-top` sorts last, so it is the newest and sits on top. It passes everything.
        table.layer_with_caps(
            &root,
            "a-bottom",
            &["exec:true"],
            &[(
                "condition_shell",
                vec![
                    Some(json!({"look": {"exec": ["true"]}, "every_s": 5})),
                    Some(json!({"fired": "the look came back"})),
                ],
            )],
        );
        table.layer_with_caps(
            &root,
            "b-top",
            &["exec:true"],
            &[("condition_shell", vec![Some(json!({"pass": true}))])],
        );
        let book = letibot_plugins::Trust::in_memory();
        book.pin("b-top", &root.join(".letibot/plugins/b-top"))
            .expect("the top layer is the operator's");
        let host = Arc::new(table_host(&root, &table).with_trust(book.clone()));
        // The premise, asserted rather than assumed: the top layer is free and the one that
        // answers is not.
        assert!(
            host.free_layer("condition_shell"),
            "the topmost declarer is pinned, which is what made the old rule wrong"
        );

        let probe = ProbeGate::refusing(|| {
            GateDecision::refuse_and_tell(
                letibot_transcript::ToolOutcome::NotRun {
                    why: "would need your approval: an unpinned layer's `exec`".into(),
                },
                "",
            )
        });
        let armed = conditions_with(&host, Some(probe.clone()))
            .arm("shell", json!({}))
            .expect("armed");
        assert_eq!(until_fired(&armed.condition), "the look came back");
        assert!(
            !probe.seen().is_empty(),
            "the answering layer is unpinned, so its look meets the gate: the top layer's pin \
             cannot stand in for the layer below it"
        );
        let seen = host.lock().rec.inputs("condition_shell").to_vec();
        let handed = seen
            .iter()
            .find(|i| !i["look"].is_null())
            .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
        assert_ne!(
            handed["look"]["exit"],
            json!(0),
            "the refused look did not run: {handed}"
        );
    }

    /// **A pin that broke while no daemon was running is still SAID.** `with_trust` re-decides every
    /// layer and `refresh_trust` records a broken pin as said as it answers it, so a host that
    /// dropped that answer would silence `plugin_untrusted` for exactly the restart case it exists
    /// for. The first tick says it.
    #[test]
    fn a_pin_that_broke_before_the_host_existed_is_said_on_its_first_tick() {
        let root = tempdir("pin-broken-restart");
        let table = Table::new();
        table.layer_with_caps(
            &root,
            "one",
            &[],
            &[("pane_dashboard", says(json!({"title": "one"})))],
        );
        let book = letibot_plugins::Trust::in_memory();
        let dir = root.join(".letibot/plugins/one");
        let pin = book.pin("one", &dir).expect("a pin");
        // The bytes move while nothing is running: a rebuild the daemon never saw.
        std::fs::write(dir.join("plugin.wasm"), b"a module, rebuilt and longer")
            .expect("a rebuild");

        // A host built NOW — the restart — is the first thing to see the broken pin.
        let host = Arc::new(table_host(&root, &table).with_trust(book.clone()));
        let hub = Hub::new("s-plugins");
        Arc::clone(&host).tick(hub.clone());

        let said = warned(&hub, "plugin_untrusted");
        assert_eq!(said.len(), 1, "the broken pin is said once: {said:?}");
        assert!(said[0].contains("`one`"), "{}", said[0]);
        assert!(
            said[0].contains(&pin.module),
            "the pinned hash: {}",
            said[0]
        );
        // And it is said ONCE: a tick every two seconds is not a wall.
        Arc::clone(&host).tick(hub.clone());
        assert_eq!(warned(&hub, "plugin_untrusted").len(), 1);
    }

    /// **A look the gate admits RUNS** — and what the gate was asked is the call a `bash` of that
    /// argv would have been, every word quoted.
    #[test]
    fn an_exec_look_the_gate_admits_runs() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("look-admitted");
        let table = shell_layer(&root, "true", json!(["true", "--a b", "$HOME"]));
        let host = Arc::new(table_host(&root, &table));
        let probe = Arc::new(ProbeGate::admitting());
        let armed = conditions_with(&host, Some(probe.clone()))
            .arm("shell", json!({}))
            .expect("armed");
        assert_eq!(until_fired(&armed.condition), "the look came back");
        let asked = probe.seen();
        assert!(
            !asked.is_empty(),
            "the gate is what decides whether an `exec` look runs"
        );
        assert!(
            asked.iter().all(|c| c == "true '--a b' '$HOME'"),
            "the gate reads the argv, quoted — not a shell line anybody could re-split: {asked:?}"
        );
        let seen = host.lock().rec.inputs("condition_shell").to_vec();
        let handed = seen
            .iter()
            .find(|i| !i["look"].is_null())
            .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
        assert_eq!(
            handed["look"]["exit"],
            json!(0),
            "an admitted look runs: {handed}"
        );
    }

    /// **A look the gate would have ASKED about does not run**, the layer is told why, and the
    /// session is told once.
    #[test]
    fn an_exec_look_the_gate_would_ask_about_does_not_run() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("look-asks");
        // A program whose running leaves a file behind, so "did not run" is a fact and not a
        // reading of the refusal.
        let marker = root.join("ran");
        let script = format!("touch {}", marker.display());
        let table = shell_layer(&root, "sh", json!(["sh", "-c", script]));
        let host = Arc::new(table_host(&root, &table));
        let hub = Hub::new("s-plugins");
        let probe = ProbeGate::refusing(|| {
            GateDecision::refuse_and_tell(
                letibot_transcript::ToolOutcome::NotRun {
                    why: "would need your approval: `bash` wants exec access to `sh -c …`".into(),
                },
                "nothing ran",
            )
        });
        let armed = conditions::HostConditions::new(
            Arc::new(Mutex::new(Some(host.clone()))),
            hub.clone(),
            Some(probe.clone()),
        )
        .arm("shell", json!({}))
        .expect("armed");
        assert_eq!(until_fired(&armed.condition), "the look came back");
        assert!(
            !marker.exists(),
            "a look the gate would ask about must not run"
        );
        let seen = host.lock().rec.inputs("condition_shell").to_vec();
        let handed = seen
            .iter()
            .find(|i| !i["look"].is_null())
            .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
        assert_eq!(handed["look"]["exit"], json!(126), "{handed}");
        let why = handed["look"]["stderr"].as_str().unwrap_or_default();
        assert!(why.contains("would need your approval"), "{why}");
        assert!(why.contains("`sh`"), "the program is named: {why}");
        let said = warned(&hub, "plugin_look");
        assert_eq!(said.len(), 1, "the session is told once: {said:?}");
        assert!(said[0].contains("shell (project)"), "{}", said[0]);
    }

    /// **A look a rule of the operator's denies does not run either**, and the sentence is the
    /// other class: somebody said no.
    #[test]
    fn an_exec_look_the_gate_denies_does_not_run() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("look-denied");
        let table = shell_layer(&root, "true", json!(["true"]));
        let host = Arc::new(table_host(&root, &table));
        let probe = ProbeGate::refusing(|| {
            GateDecision::refuse_and_tell(
                letibot_transcript::ToolOutcome::Denied {
                    req_id: "adj-1-0001".into(),
                },
                "REFUSED by the permission config",
            )
        });
        let armed = conditions_with(&host, Some(probe.clone()))
            .arm("shell", json!({}))
            .expect("armed");
        assert_eq!(until_fired(&armed.condition), "the look came back");
        let seen = host.lock().rec.inputs("condition_shell").to_vec();
        let handed = seen
            .iter()
            .find(|i| !i["look"].is_null())
            .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
        let why = handed["look"]["stderr"].as_str().unwrap_or_default();
        assert!(why.contains("the gate denied it"), "{why}");
        assert!(why.contains("REFUSED by the permission config"), "{why}");
    }

    /// **The notice is once per (layer, program, reason)** — a watch polls every ninety seconds,
    /// and the same refusal is not news the second time. A refusal that CHANGES is.
    #[test]
    fn a_refused_look_is_said_once_until_it_changes() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("look-once");
        // One answer that both asks for the look and fires on it, so every arming asks the gate
        // exactly once — the shape a watch repeating its interval has.
        let table = Table::new();
        table.layer_with_caps(
            &root,
            "shell",
            &["exec:true"],
            &[(
                "condition_shell",
                says(json!({
                    "look": {"exec": ["true"]},
                    "every_s": 5,
                    "fired": "the look came back"
                })),
            )],
        );
        let host = Arc::new(table_host(&root, &table));
        let hub = Hub::new("s-plugins");
        let probe = ProbeGate::refusing(|| {
            GateDecision::refuse_and_tell(
                letibot_transcript::ToolOutcome::NotRun {
                    why: "would need your approval: the first reason".into(),
                },
                "nothing ran",
            )
        });
        let conditions = conditions::HostConditions::new(
            Arc::new(Mutex::new(Some(host.clone()))),
            hub.clone(),
            Some(probe.clone()),
        );
        for _ in 0..3 {
            conditions.arm("shell", json!({})).expect("armed");
        }
        assert_eq!(
            probe.seen().len(),
            3,
            "each arming asked the gate, which is what makes the count below mean something"
        );
        assert_eq!(
            warned(&hub, "plugin_look").len(),
            1,
            "three firings of one refusal are one warning: {:?}",
            warned(&hub, "plugin_look")
        );

        // **And a reason that changed is news again** — the layer is fixed, or the gate's own
        // reason moved, and silence would leave the session believing the first sentence. The
        // same host, so the only thing that differs is the sentence.
        let other = ProbeGate::refusing(|| {
            GateDecision::refuse_and_tell(
                letibot_transcript::ToolOutcome::NotRun {
                    why: "would need your approval: a different reason".into(),
                },
                "nothing ran",
            )
        });
        let same = conditions::HostConditions::new(
            Arc::new(Mutex::new(Some(host.clone()))),
            hub.clone(),
            Some(other),
        );
        let armed = same.arm("shell", json!({})).expect("armed");
        assert_eq!(until_fired(&armed.condition), "the look came back");
        assert_eq!(
            warned(&hub, "plugin_look").len(),
            2,
            "a second reason is a second sentence: {:?}",
            warned(&hub, "plugin_look")
        );
    }

    /// **A session with no gate runs no program a layer declared** — *no gate* is not *every gate
    /// open*, which is the direction the whole entry fails in.
    #[test]
    fn a_session_with_no_gate_runs_no_exec_look() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("look-no-gate");
        let table = shell_layer(&root, "true", json!(["true"]));
        let host = Arc::new(table_host(&root, &table));
        let armed = conditions(&host).arm("shell", json!({})).expect("armed");
        assert_eq!(until_fired(&armed.condition), "the look came back");
        let seen = host.lock().rec.inputs("condition_shell").to_vec();
        let handed = seen
            .iter()
            .find(|i| !i["look"].is_null())
            .expect("the look's result was handed back");
        assert_eq!(handed["look"]["exit"], json!(126), "{handed}");
        assert!(
            handed["look"]["stderr"]
                .as_str()
                .is_some_and(|s| s.contains("no gate")),
            "{handed}"
        );
    }

    /// A gate that admits everything — the fixture the deferred look needs, since what this test is
    /// about is the MUTEX and not the decision.
    struct Admits;

    impl letibot_tools::Gate for Admits {
        fn admit(&mut self, _call: &letibot_tools::GateCall<'_>) -> GateDecision {
            GateDecision::Admit
        }
    }

    /// **A look is DEFERRED while the session's gate is busy with somebody else's card** — not
    /// blocked, and not refused.
    ///
    /// `ToolRuntime::run` holds the gate's mutex across the model's own decision, card and all, on
    /// purpose — for minutes. A look that waited on it would park the session's one monitor thread,
    /// and every other watch on it, behind that card; so the look takes the lock with `try_lock`,
    /// and a busy gate is *not now*: nothing runs, the layer is told nothing that reads as a
    /// refusal, no notice goes to the session, and the watch asks again next tick.
    #[test]
    fn an_exec_look_is_deferred_while_the_gate_is_busy() {
        use letibot_tools::exec::plugin_condition::PluginConditions as _;
        let root = tempdir("look-busy");
        // A program whose running leaves a file behind, so "did not run" is a fact.
        let marker = root.join("ran");
        let script = format!("touch {}", marker.display());
        let table = shell_layer(&root, "sh", json!(["sh", "-c", script]));
        let host = Arc::new(table_host(&root, &table));
        let hub = Hub::new("s-plugins");

        // **The REAL runtime and the REAL `Unasked` impl**, because the deferral IS `try_lock` on
        // this mutex: a fixture gate would prove nothing about it.
        let backend = tempdir("look-busy-backend");
        let runtime = Arc::new(
            letibot_tools::ToolRuntime::new(
                letibot_tools::Registry::new(),
                Box::new(letibot_tools::HostBackend::new(&backend).expect("a backend")),
            )
            .with_gate(Box::new(Admits)),
        );
        let gate: Arc<dyn conditions::Unasked> = runtime.clone();
        let conditions = conditions::HostConditions::new(
            Arc::new(Mutex::new(Some(host.clone()))),
            hub.clone(),
            Some(gate),
        );

        // **Hold the lock exactly as the model's own decision does**, and fire the look from
        // another thread: it must come back promptly, with nothing run and nothing said.
        let held = runtime.gate.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let conditions = conditions.clone();
            std::thread::spawn(move || {
                let armed = conditions.arm("shell", json!({})).expect("armed");
                let _ = tx.send(armed.condition.met());
            });
        }
        let said = rx.recv_timeout(Duration::from_secs(5)).expect(
            "a look waited on a gate another thread is holding — that is the session's monitor \
             thread parked behind somebody's card",
        );
        assert_eq!(said, None, "a deferred look is not a firing");
        assert!(
            !marker.exists(),
            "a look ran while the gate was busy deciding somebody else's call"
        );
        assert!(
            warned(&hub, "plugin_look").is_empty(),
            "a busy gate is not news for the plugin: {:?}",
            warned(&hub, "plugin_look")
        );
        assert!(
            host.lock()
                .rec
                .inputs("condition_shell")
                .iter()
                .all(|i| i["look"].is_null()),
            "a deferred look was handed to the layer as a result"
        );

        // **And with the gate free the same look runs**, which is what makes the assertions above
        // about the LOCK rather than about a look that never works. A fresh host, because the
        // fixture's script is spent one answer per arming: the deferred arming took the one that
        // declared the look.
        drop(held);
        let table = shell_layer(&root, "sh", json!(["sh", "-c", script]));
        let host = Arc::new(table_host(&root, &table));
        let gate: Arc<dyn conditions::Unasked> = runtime.clone();
        let conditions =
            conditions::HostConditions::new(Arc::new(Mutex::new(Some(host))), hub, Some(gate));
        let armed = conditions.arm("shell", json!({})).expect("armed");
        assert_eq!(until_fired(&armed.condition), "the look came back");
        assert!(
            marker.exists(),
            "the look did not run once the gate was free"
        );
    }

    // -----------------------------------------------------------------------------------------
    // The room's pane: the feed the daemon builds, and the four keys it performs
    // -----------------------------------------------------------------------------------------

    /// A project layer that answers `pane_room`, declares the four controls, and reads the `room`
    /// feed — the manifest a project would write over the built-in.
    fn room_project(tag: &str, answer: &str, actions: &str) -> PathBuf {
        let root = tempdir(tag);
        let layer = root.join(".letibot/plugins/room");
        std::fs::create_dir_all(&layer).expect("the layer dir");
        std::fs::write(
            layer.join("plugin.toml"),
            format!(
                "name = \"room\"\npurpose = \"the room\"\nhooks = [\"pane_room\"]\n\
                 capabilities = []\nfeeds = [\"room\"]\nactions = [{actions}]\ncontract = 1\n"
            ),
        )
        .expect("a manifest");
        std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
        std::fs::write(layer.join("answer.json"), answer).expect("an answer");
        std::fs::write(layer.join("second.json"), answer).expect("a second answer");
        root
    }

    /// A real room on a socket — the thing a room pane is drawn from.
    fn a_room(tag: &str) -> (PathBuf, letibot_room::Server) {
        let dir = tempdir(tag);
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        (dir, server)
    }

    /// **Wait for the pane's picture of the room to catch up.** The feed is read on a thread of its
    /// own, once a second, so a test waits for it rather than sleeping a fixed second — and the
    /// pane's own *not read yet* frame is a fact, not a failure.
    fn until_the_room_is_read(link: &crate::room::link::SessionRoom, items: usize) {
        use crate::plugins::room_feed::RoomFeed as _;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snap = link.snapshot();
            if snap.unreadable.is_none() && snap.items.len() == items {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the room was never read: {:?}",
                snap.unreadable
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// **A module that answers the same JSON every time** — for a test that presses more keys than
    /// it scripted frames. [`Rounds`] is the right tool when the ANSWER is the subject; here the
    /// subject is what the host does with a row, and the row is the same row every time.
    struct Same {
        hooks: Vec<String>,
        answer: String,
    }

    impl Module for Same {
        fn hooks(&self) -> Vec<String> {
            self.hooks.clone()
        }

        fn call(&self, _hook: &str, _input: &str) -> Result<Option<String>, String> {
            Ok(Some(self.answer.clone()))
        }

        fn fuel(&self) -> u64 {
            7
        }
    }

    struct SameLoader;

    impl Loader for SameLoader {
        fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            let dir = wasm.parent().unwrap_or(Path::new("."));
            let manifest = Manifest::read(dir)?;
            let answer = std::fs::read_to_string(dir.join("answer.json"))
                .map_err(|e| format!("answer.json: {e}"))?;
            Ok(Box::new(Same {
                hooks: manifest.hooks.clone(),
                answer,
            }))
        }
    }

    /// **A row's key is performed by the HOST, and the feed it was drawn from is the room's.**
    ///
    /// This is the slice in one test: the daemon builds the `room` feed (so the layer is told what
    /// is queued and who holds it), the layer answers a row carrying `{"verb":"claim","id":"i1"}`,
    /// and the daemon performs it — the room's own state machine is what moves, over the session's
    /// own connection.
    #[test]
    fn a_room_panes_key_is_performed_by_the_host() {
        let (dir, server) = a_room("room-key");
        // An item somebody else posted, so `s-room-pane` may claim the head of `ram`.
        let other = letibot_room::client::Client::connect(
            server.path(),
            letibot_room::Speaker::top_level("s-b"),
            Arc::new(|_| {}),
        )
        .expect("s-b connects");
        other
            .request(&letibot_room::Request::Post {
                id: "p1".into(),
                tag: "ram".into(),
                body: "8 GB".into(),
            })
            .expect("a post");
        let link = crate::room::link::SessionRoom::connect(
            "s-room-pane",
            server.path(),
            Arc::new(|_: &str| {}),
            None,
        )
        .expect("the session connects");
        until_the_room_is_read(&link, 1);

        let answer = r#"{"title":"the room","panels":[{"id":"queues","title":"queues",
            "summary":[{"tree":{"rows":[{"id":"i1","cells":["8 GB"],
            "enter":{"verb":"claim","id":"i1"}}]}}]}]}"#;
        let root = room_project(
            "room-key",
            answer,
            "\"claim\", \"release\", \"abandon\", \"rule\"",
        );
        let hub = Hub::new("s-room-pane");
        let host = PluginHost::discover(
            "s-room-pane",
            &root,
            None,
            None,
            Arc::new(SameLoader),
            Arc::new(FileBuilder),
        )
        .with_room(link);

        host.open("s-room-pane", &hub, "room/room", (80, 24));
        // **The feed the layer was handed is the room's** — the item, its tag, and the room's own
        // identity sentence.
        let inputs = host.lock().rec.inputs("pane_room").to_vec();
        assert_eq!(inputs.len(), 1, "one render, one call");
        let feed = &inputs[0]["feeds"]["room"];
        assert_eq!(feed["here"], json!("s-room-pane"));
        assert!(
            feed["identity"]
                .as_str()
                .unwrap_or_default()
                .contains("no store"),
            "{feed}"
        );
        assert_eq!(feed["tags"][0]["tag"], json!("ram"));
        assert_eq!(feed["tags"][0]["rows"][0]["entry"], json!("i1"));
        assert_eq!(feed["tags"][0]["rows"][0]["claimable"], json!(true));

        // **And the key moves the room.** The layer said *claim entry i1*; the daemon performed it.
        host.action("s-room-pane", &hub, "room/room", "i1", "claim");
        assert_eq!(
            server.items()[0].holder(),
            Some("s-room-pane"),
            "the claim is on the session whose pane it is"
        );
        // The second render is the pane again, with the state the key made.
        let inputs = host.lock().rec.inputs("pane_room").to_vec();
        assert_eq!(inputs.len(), 2);
        assert_eq!(
            inputs[1]["feeds"]["room"]["tags"][0]["rows"][0]["claimant"],
            json!("s-room-pane"),
            "the frame after the key is the state the key made"
        );

        // **A key the room refuses is SAID** — a key that did nothing is indistinguishable from
        // one that was not pressed. `#2` is somebody else's, so a release of it is `NotHeld`.
        other
            .request(&letibot_room::Request::Post {
                id: "p2".into(),
                tag: "disk".into(),
                body: "40 GB".into(),
            })
            .expect("a second post");
        host.action("s-room-pane", &hub, "room/room", "i2", "release");
        let said = hub.retained();
        assert!(
            said.iter().any(|e| matches!(
                &e.event,
                SessionEvent::Warning { code, detail, .. }
                    if code == "room_act_refused" && detail.contains("does not hold")
            )),
            "the room's refusal reaches the person: {:?}",
            said.iter()
                .map(|e| format!("{:?}", e.event))
                .collect::<Vec<_>>()
        );

        drop(other);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The pane's restart is a FRAME to the supervisor, and its outcome is said.**
    ///
    /// The control is not a room verb: the layer names `restart`, and the HOST asks the supervisor
    /// that starts coordinators. This is the whole requirement in one test — the act reaches the
    /// supervisor with the session's name on it, the outcome reaches the person as a notice, and the
    /// note the act left is in the feed the next frame is drawn from.
    #[test]
    fn the_restart_control_asks_the_supervisor_and_its_outcome_is_said() {
        use crate::room::supervise::{RestartFrame, RoomSupervisor};
        use std::sync::Mutex as StdMutex;

        struct Ask(StdMutex<Vec<RestartFrame>>);
        impl RoomSupervisor for Ask {
            fn restart(&self, frame: &RestartFrame) -> Result<String, String> {
                self.0.lock().unwrap().push(frame.clone());
                Ok("the room was stopped and started again by a fixture".to_string())
            }
        }

        let (dir, server) = a_room("room-restart");
        let asked = Arc::new(Ask(StdMutex::new(Vec::new())));
        let link = crate::room::link::SessionRoom::connect(
            "s-room-pane",
            server.path(),
            Arc::new(|_: &str| {}),
            Some(asked.clone()),
        )
        .expect("the session connects");
        until_the_room_is_read(&link, 0);

        // The layer answers ONE row: the restart, which is the fifth verb and not a room act.
        let answer = r#"{"title":"the room","panels":[{"id":"server","key":"s",
            "title":"the server","summary":[{"tree":{"rows":[{"id":"restart","cells":["restart"],
            "enter":{"verb":"restart","id":"restart"}}]}}]}]}"#;
        let root = room_project(
            "room-restart",
            answer,
            "\"claim\", \"release\", \"abandon\", \"rule\", \"restart\"",
        );
        let hub = Hub::new("s-room-pane");
        let host = PluginHost::discover(
            "s-room-pane",
            &root,
            None,
            None,
            Arc::new(SameLoader),
            Arc::new(FileBuilder),
        )
        .with_room(link);
        host.open("s-room-pane", &hub, "room/room", (80, 24));

        // **The act runs off the connection's thread** (a launcher that overran its minute must not
        // leave the person's own keys queued behind it), so the test waits for the frame rather
        // than reading it a line later.
        host.action("s-room-pane", &hub, "room/room", "restart", "restart");
        let until = |what: &str, f: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if f() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("{what} never happened");
        };
        until("the supervisor is asked", &|| {
            !asked.0.lock().unwrap().is_empty()
        });
        // And a second press while one is running is refused rather than queued: one room, one
        // restart.
        let frames = asked.0.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "one press, one frame");
        assert_eq!(frames[0].by, "s-room-pane");
        assert!(
            frames[0].why.contains("pressed restart in the room pane"),
            "{}",
            frames[0].why
        );
        assert!(
            !frames[0].was_running,
            "the fixture's socket has no lease, so nothing was running to stop"
        );

        // **The outcome is said**, not swallowed: a key whose result nobody printed is a key a
        // person cannot trust.
        until("the outcome is said", &|| {
            !warned(&hub, "room_restarted").is_empty()
        });
        // **And the note is in the next frame the pane is drawn from** — when, and why. The pane's
        // own clock (`every_s`) is what asks for it in production; the test asks, which is the same
        // render on the same path.
        host.open("s-room-pane", &hub, "room/room", (80, 24));
        let inputs = host.lock().rec.inputs("pane_room").to_vec();
        let feed = &inputs.last().expect("a render after the key")["feeds"]["room"];
        assert_eq!(feed["restart"]["available"], json!(true));
        let note = &feed["restart"]["marker"];
        assert_eq!(
            note["cause"],
            json!("daemon"),
            "nothing was running: {note}"
        );
        assert_eq!(note["by"], json!("s-room-pane"));
        assert!(
            note["sentence"]
                .as_str()
                .unwrap_or_default()
                .contains("no items at all"),
            "{note}"
        );

        // **A supervisor that refuses is a notice too** — the middle register, with the reason.
        struct Refuse;
        impl RoomSupervisor for Refuse {
            fn restart(&self, _frame: &RestartFrame) -> Result<String, String> {
                Err("`letibot --room --stop` answered exit status: 1: no room on this box".into())
            }
        }
        let link = crate::room::link::SessionRoom::connect(
            "s-room-pane",
            server.path(),
            Arc::new(|_: &str| {}),
            Some(Arc::new(Refuse)),
        )
        .expect("the session connects");
        until_the_room_is_read(&link, 0);
        let host = PluginHost::discover(
            "s-room-pane",
            &root,
            None,
            None,
            Arc::new(SameLoader),
            Arc::new(FileBuilder),
        )
        .with_room(link);
        let hub = Hub::new("s-room-pane");
        host.open("s-room-pane", &hub, "room/room", (80, 24));
        host.action("s-room-pane", &hub, "room/room", "restart", "restart");
        until("the refusal is said", &|| {
            !warned(&hub, "room_restart_refused").is_empty()
        });
        let refused = warned(&hub, "room_restart_refused");
        assert!(
            refused.iter().any(|d| d.contains("no room on this box")),
            "{refused:?}"
        );

        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A row carrying a verb the manifest did not declare is refused before it is drawn** — the
    /// gate `View::validate_for` keeps, on the daemon's side of the wire.
    #[test]
    fn a_room_row_reaching_an_undeclared_verb_is_refused() {
        let answer = r#"{"title":"the room","panels":[{"id":"queues","title":"queues",
            "summary":[{"tree":{"rows":[{"id":"i1","cells":["8 GB"],
            "enter":{"verb":"veto","id":"i1"}}]}}]}]}"#;
        // The manifest declares the four, and `veto` is not one of them.
        let root = room_project(
            "room-veto",
            answer,
            "\"claim\", \"release\", \"abandon\", \"rule\"",
        );
        let hub = Hub::new("s-plugins");
        let host = PluginHost::discover(
            "s-plugins",
            &root,
            None,
            None,
            Arc::new(RoundLoader),
            Arc::new(FileBuilder),
        );
        host.open("s-plugins", &hub, "room/room", (80, 24));
        let published = views(&hub);
        let (_, view) = published.last().expect("a frame");
        assert_eq!(
            view["panels"][0]["unavailable"]["why"]
                .as_str()
                .unwrap_or_default()
                .contains("does not declare that action"),
            true,
            "the pane says why instead of drawing the row: {view}"
        );
        assert!(
            !warned(&hub, "plugin_view_refused").is_empty(),
            "and the session's model is told once"
        );

        // **The restart is a declared action like the four, and it is refused like them too.** A
        // project layer that ships its own room pane and does NOT list `restart` gets its row
        // refused: the gate is the manifest, not the verb's familiarity. This is the half that
        // makes *declared* mean something — the built-in's manifest lists five, and a layer that
        // lists four cannot reach the fifth.
        let answer = r#"{"title":"the room","panels":[{"id":"server","key":"s",
            "title":"the server","summary":[{"tree":{"rows":[{"id":"restart","cells":["restart"],
            "enter":{"verb":"restart","id":"restart"}}]}}]}]}"#;
        let root = room_project(
            "room-no-restart",
            answer,
            "\"claim\", \"release\", \"abandon\", \"rule\"",
        );
        let hub = Hub::new("s-plugins");
        let host = PluginHost::discover(
            "s-plugins",
            &root,
            None,
            None,
            Arc::new(RoundLoader),
            Arc::new(FileBuilder),
        );
        host.open("s-plugins", &hub, "room/room", (80, 24));
        let published = views(&hub);
        let (_, view) = published.last().expect("a frame");
        assert!(
            view["panels"][0]["unavailable"]["why"]
                .as_str()
                .unwrap_or_default()
                .contains("does not declare that action"),
            "an undeclared `restart` is refused by the same gate: {view}"
        );
    }

    /// **The committed `room` module RUNS**, against a real room, through the daemon's own host.
    ///
    /// The rake this exists for is measured: `plugins/examples/plan-dashboard`'s committed `.wasm`
    /// had never been executed and trapped on its first call
    /// (`.letibot/notes/the-pane-contracts-two-look-keys.md`). A guest that compiles for
    /// `wasm32-unknown-unknown` is not a guest that has run, so this test loads the module the
    /// binary carries, asks it for a pane with a real room behind it, and asserts the answer is the
    /// LAYER's — the tag, the item's body, and the room's identity — rather than the host's
    /// `unavailable` panel.
    #[test]
    fn the_builtin_room_layer_answers_its_pane_with_a_real_room_behind_it() {
        let (dir, server) = a_room("room-builtin");
        let other = letibot_room::client::Client::connect(
            server.path(),
            letibot_room::Speaker::top_level("s-b"),
            Arc::new(|_| {}),
        )
        .expect("s-b connects");
        other
            .request(&letibot_room::Request::Post {
                id: "p1".into(),
                tag: "ram".into(),
                body: "8 GB, ~10 min".into(),
            })
            .expect("a post");
        let link = crate::room::link::SessionRoom::connect(
            "s-room-builtin",
            server.path(),
            Arc::new(|_: &str| {}),
            None,
        )
        .expect("the session connects");
        until_the_room_is_read(&link, 1);

        let workspace = tempdir("room-builtin-ws");
        let host = PluginHost::production("s-room-builtin", &workspace).with_room(link);
        let hub = Hub::new("s-room-builtin");
        // **Nothing is compiled until a head looks.** The room's module is 218 KB and costs
        // 1.4–2.0 s of CPU in a debug host, so a session that nobody attaches to must not pay for
        // it — `ensure_the_room_pane` is called from `attach` and from this door.
        assert!(
            host.panes().is_empty(),
            "a session with no head has no pane: {:?}",
            host.panes()
        );
        host.open("s-room-builtin", &hub, "room/room", (100, 30));
        assert_eq!(
            host.panes(),
            vec!["room/room".to_string()],
            "and the door that asks for it is the door that installs it"
        );

        let (pane, view) = views(&hub)
            .into_iter()
            .filter(|(p, _)| p == "room/room")
            .next_back()
            .expect("the pane answered");
        assert_eq!(pane, "room/room");
        let drawn = serde_json::to_string(&view).unwrap();
        assert!(
            view["panels"][0].get("unavailable").is_none(),
            "the layer answered, rather than the host's refusal panel: {drawn}"
        );
        assert!(drawn.contains("`ram`"), "the tag is drawn: {drawn}");
        assert!(
            drawn.contains("8 GB, ~10 min"),
            "the item is drawn: {drawn}"
        );
        assert!(
            drawn.contains("no store"),
            "and the room's own identity, which is what keeps an empty queue from reading as a \
             quiet one: {drawn}"
        );
        // **The row's key is the manifest's verb**, so a press would reach the host's act.
        assert!(drawn.contains("\"verb\":\"claim\""), "{drawn}");

        drop(other);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
