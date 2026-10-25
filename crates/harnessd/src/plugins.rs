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
//! There is **no `PaneClose` on the wire** (C4), so a pane a head has opened stays in this host
//! and keeps being refreshed at its own `every_s`. Nothing leaks but the refresh: a read look and
//! a wasm call every 5–30 seconds while the session is up, for a screen somebody may have closed.
//! A close frame is the fix, and it is a contract change this package does not make.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use letibot_plugins::{
    Builder, Capability, CargoBuilder, Failure, Loader, Module, Recorder, Stack,
};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::pane::{Block, Line, LookSpec, Panel, Role, Unavailable, View};
use letibot_sessionlog::registry::PaneDriver;
use serde_json::{Value, json};

pub mod look;
pub mod rano;

/// **How often the hot-reload tick looks** — `docs/plugins-plan.md` §2.3 makes a change take
/// effect on the next hook call, and this is the longest a person waits after saving one. Two
/// seconds is `mergequeue::watch`'s own poll, for the same reason: a stat per layer is free.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// The rectangle a pane is asked to draw at when nobody told the host one. `ClientFrame::PaneOpen`
/// carries no size (C4) and nothing else on the wire does either, so a dashboard laid out for a
/// wide screen is the honest default rather than a guess about a head's terminal.
pub const SIZE_W: u32 = 120;
pub const SIZE_H: u32 = 40;

/// The contract version every pane input carries (C3): the host says which one it speaks.
const PANE_V: u32 = 1;

/// **The name a pane's own `look` travels under inside the stack.**
///
/// C2's answer protocol and C3's pane document both use the top-level key `look` and they mean
/// different things: to the stack it is *ask the layer below* (`{"look":{"below":…}}`), to a pane
/// it is *run this for me* (`{"look":{"read":"…"}}`). A pane answer read by the stack as an
/// answer protocol would meet `look` without `below`, be recorded as a broken look below and
/// passed to the next layer down — so a dashboard that asks for a file would answer nothing.
///
/// The collision is in the two contracts and neither is this package's to change, so it is
/// bridged at the one seam that owns both: every module this host loads is wrapped so a pane
/// hook's `look` that names **no layer below** is re-spelled on the way out ([`spell`]) and
/// re-spelled back on the way in ([`unspell`]). The plugin and the stack each keep their own
/// reading of the word, and the daemon is where the two are the same document.
const PANE_LOOK: &str = "pane_look";

/// One session's layers, its recorded inputs, and the panes a head has opened.
pub struct PluginHost {
    session: String,
    workspace: PathBuf,
    inner: Mutex<Inner>,
    /// How the gate's step 1 is performed. `CargoBuilder` in the daemon; a table of answers in a
    /// test, because a test that ran cargo would need the wasm target and a real crate to build.
    builder: Arc<dyn Builder>,
}

struct Inner {
    stack: Stack,
    rec: Recorder,
    /// The panes a head has opened, by id, with what the pane returned and when it wants asking
    /// again.
    panes: BTreeMap<String, Open>,
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
    size: (u32, u32),
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
            // **The spelling seam** — see [`PANE_LOOK`]: every module is wrapped so a pane's own
            // `look` survives the stack's look-below protocol.
            Arc::new(Spelled { inner: loader }),
        );
        PluginHost {
            session: session.into(),
            workspace,
            inner: Mutex::new(Inner {
                stack,
                rec: Recorder::new(),
                panes: BTreeMap::new(),
                gating: BTreeSet::new(),
                said: BTreeSet::new(),
                notices: Vec::new(),
                poll_at: Instant::now() + POLL_EVERY,
            }),
            builder,
        }
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
        let session_dir = crate::mergequeue::git::letibot_data_dir()
            .map(|dir| dir.join("sessions").join(session).join("plugins"));
        PluginHost::discover(
            session,
            workspace,
            session_dir,
            user_config_dir(),
            loader,
            builder,
        )
    }

    /// This host as the daemon uses it: rano loads the modules and cargo builds them.
    pub fn production(session: &str, workspace: &Path) -> PluginHost {
        PluginHost::for_session(
            session,
            workspace,
            Arc::new(rano::RanoLoader),
            Arc::new(CargoBuilder),
        )
    }

    pub fn session(&self) -> &str {
        &self.session
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
                    self.after_apply(hub);
                }
            }
        }
        applied
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

    /// **A candidate passed: re-render what is on a screen, and say which panes are gone.**
    fn after_apply(&self, hub: &Arc<Hub>) {
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
        for pane in open {
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

    /// Every stack failure not yet said, once each.
    fn say_failures_now(&self, hub: &Arc<Hub>) {
        let mut inner = self.lock();
        let failures: Vec<Failure> = inner.stack.failures().to_vec();
        say_failures(hub, &mut inner, &failures);
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
        let mut interior = self.lock();
        let size = interior
            .panes
            .get(pane)
            .map(|p| p.size)
            .unwrap_or((SIZE_W, SIZE_H));
        let mut state = interior.panes.get(pane).and_then(|p| p.state.clone());
        let mut req = req;
        for step in 0..2 {
            let input = input_value(&state, &req, size);
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
            let answer = unspell(&answer);
            let by = resolved.by.clone().unwrap_or_else(|| manifest.to_string());
            let (published, view) = match validated(&answer, pane, &by) {
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
                    size,
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

impl PaneDriver for PluginHost {
    fn attach(&self, _session_id: &str, hub: &Arc<Hub>) {
        self.announce(hub);
    }

    fn open(&self, _session_id: &str, hub: &Arc<Hub>, pane: &str) {
        self.render(hub, pane, Req::fresh());
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

/// C3's input, once: the state the pane returned, the look's result, the key that was pressed, and
/// the rectangle. Every field is present, because a plugin deserialises this into a struct and a
/// missing key is a plugin that cannot tell *no look* from *an empty one*.
fn input_value(state: &Option<Value>, req: &Req, (w, h): (u32, u32)) -> Value {
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
    })
}

/// **The plugin's answer, as a view this host can drive** — or the sentence saying why not.
fn validated(answer: &Value, pane: &str, by: &str) -> Result<View, String> {
    let view: View = serde_json::from_value(answer.clone()).map_err(|e| {
        format!("the answer is not a C3 view ({e}), while `{pane}` was resolved from `{by}`")
    })?;
    view.validate()?;
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

/// **The pane's own `look`, put back under the name C3 gives it** — see [`PANE_LOOK`].
fn unspell(answer: &Value) -> Value {
    let mut answer = answer.clone();
    if let Some(map) = answer.as_object_mut()
        && !map.contains_key("look")
        && let Some(look) = map.remove(PANE_LOOK)
    {
        map.insert("look".to_string(), look);
    }
    answer
}

/// **The same, the other way** — a pane hook's `look` that names no layer below becomes
/// [`PANE_LOOK`] so the stack's answer protocol does not read it as *ask below*. A real look
/// below is untouched: it carries `below`, which no `LookSpec` can.
fn spell(hook: &str, text: &str) -> String {
    if !hook.starts_with("pane_") {
        return text.to_string();
    }
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        // Not JSON is the stack's failure to record, not this seam's.
        return text.to_string();
    };
    let Some(map) = value.as_object_mut() else {
        return text.to_string();
    };
    let spec = matches!(map.get("look"), Some(Value::Object(l)) if !l.contains_key("below"));
    if !spec {
        return text.to_string();
    }
    let look = map.remove("look").expect("just checked");
    map.insert(PANE_LOOK.to_string(), look);
    value.to_string()
}

/// **A stack whose modules speak C3's `look`, not C2's** — see [`PANE_LOOK`].
struct Spelled {
    inner: Arc<dyn Loader>,
}

impl Loader for Spelled {
    fn load(&self, wasm: &Path, caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        Ok(Box::new(SpelledModule {
            inner: self.inner.load(wasm, caps)?,
        }))
    }
}

struct SpelledModule {
    inner: Box<dyn Module>,
}

impl Module for SpelledModule {
    fn hooks(&self) -> Vec<String> {
        self.inner.hooks()
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        Ok(self.inner.call(hook, input)?.map(|text| spell(hook, &text)))
    }

    fn fuel(&self) -> u64 {
        self.inner.fuel()
    }
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

/// `$XDG_CONFIG_HOME/letibot`, else `~/.config/letibot` — where the user scope's layers are.
fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|base| base.join("letibot"))
}

impl PluginHost {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_plugins::{BuildError, Manifest, Module};
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
        assert_eq!(inputs[1]["v"], json!(1));
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
}
