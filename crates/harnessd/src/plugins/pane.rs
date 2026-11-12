//! The pane half of a session's host: which panes a stack exports, the frame each open pane is
//! handed next, the look a frame asks the host to run, and what a head's keys do.
//! The view contract itself belongs to `letibot_plugin::pane`, and the daemon only drives it.

use super::*;

impl PluginHost {
    /// The trees a look reads in: the session's checkout first, then the tree it stands in.
    pub fn look_trees(&self) -> look::Trees<'_> {
        look::Trees::new(&self.checkout, &self.workspace)
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

    /// The stack changed: bring every screen into line with it. A pane still there is asked again
    /// with its look (a reload is a new version of the answer, not a reason to redraw the frame that
    /// asks for the file), a gone one is published as `PluginPaneGone`, and a new one is announced.
    pub(super) fn refresh_panes(&self, hub: &Arc<Hub>) {
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
    pub(super) fn refresh_due(&self, hub: &Arc<Hub>) {
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
    pub(super) fn say_failures_now(&self, hub: &Arc<Hub>) {
        let mut inner = self.lock();
        let failures: Vec<Failure> = inner.stack.failures().to_vec();
        say_failures(hub, &mut inner, &failures);
    }

    /// The feeds a pane is handed, by the answering layer's own declaration - and only those. A
    /// layer declaring a feed the host does not build is handed no key, not an empty list, which
    /// would say there are none. Public because a test asks the daemon what it would hand over.
    pub fn feeds_for(&self, hook: &str) -> BTreeMap<String, Value> {
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
                letibot_plugin::pane::Feed::Github => {
                    // The daemon's seat, and only when a scope is attached: a pane handed no key
                    // says *no scope is attached*, which an empty scope list would deny.
                    if let Some(source) = self.github.as_ref()
                        && let Some(seat) = source.github()
                    {
                        out.insert(want.name().to_string(), seat);
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
    pub(super) fn render(&self, hub: &Arc<Hub>, pane: &str, req: Req) {
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
        // What the pane's next call is handed, read before the call and not across it: the module
        // runs with the lock released, so a frame drawn a moment later is not this answer's.
        let (size, mut state) = {
            let interior = self.lock();
            (
                interior
                    .sizes
                    .get(pane)
                    .copied()
                    .unwrap_or((SIZE_W, SIZE_H)),
                interior.panes.get(pane).and_then(|p| p.state.clone()),
            )
        };
        let mut req = req;
        for step in 0..2 {
            let input = input_value(&state, &req, size, &feeds);
            let Some(resolved) = self.call_unlocked(hub, &hook, &input) else {
                // The layers moved on while this frame was being drawn. Nothing is published: the
                // next refresh asks the stack that is in force instead of drawing this answer.
                return;
            };
            let mut interior = self.lock();
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
                // A look reads the checkout, and a filesystem walk is not something this
                // session's other panes and turns wait for.
                drop(interior);
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

impl PaneDriver for PluginHost {
    fn attach(&self, _session_id: &str, hub: &Arc<Hub>) {
        // A head arriving is the first moment a built-in pane is worth a compile, but the compile
        // costs seconds, so it is only asked for here and runs on the next tick's thread.
        self.builtins_wanted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.announce(hub);
    }

    fn open(&self, _session_id: &str, hub: &Arc<Hub>, pane: &str, size: (u32, u32)) {
        // A head that asks for a pane without having been announced one still gets it. Only then:
        // compiling every built-in first holds a pane the stack already exports behind seconds of
        // compiles the tick's thread is doing anyway.
        if !self.panes().iter().any(|p| p == pane) {
            self.ensure_builtin_panes();
        }
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
