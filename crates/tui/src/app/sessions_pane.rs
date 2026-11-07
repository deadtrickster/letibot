//! The session list, drawn from a plugin: the built-in `sessions` layer decides the groups, names,
//! counts and keys, and the head draws its answer through rano's one tree. The head runs the layer
//! in process, because the list is how a person gets out of a broken session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_plugin::pane::{Act as RowAct, Feed, Input};
use letibot_plugins::{Capability, Loader, Module, Stack};
use letibot_sessionlog::pane as pv;
use rano::agent::pane::PaneLines;
use rano::agent::tree_list::{
    Action as TreeAction, Cell as TreeCell, Fold, Row as TreeRow, TreeList,
};
use serde_json::{Value, json};

use crate::app::*;

/// The hook this pane answers under.
pub(crate) const HOOK: &str = "pane_sessions";

/// The list's frame, in the head's own words - the tree the head draws without a layer
/// ([`App::session_list`]) carries exactly this title, and a plugin's list that looked different
/// because a layer answered it would make *which layer drew this* a guess.
const TITLE: &str = "sessions in this daemon";

/// The keys that are the HEAD's, whatever a layer says. The row-dependent half of the hint (`enter
/// switches`) is the selected row's own act and comes from the answer.
const HINT: [&str; 5] = [
    "↑↓ moves",
    "or type a number or part of a name and press enter",
    "/new [title] makes one",
    "/rename NAME names this one",
    "esc closes",
];

const NOTE: &str = "switching does not stop anything: a turn keeps running in the session you \
                    left, and it is still there when you come back.";

/// What the list says when the daemon holds nothing - the same sentence the built-in layer answers
/// with, so an empty list is one picture whichever of the two drew it.
const EMPTY: &str = "none listed yet — the daemon has not answered, or this head is replaying a \
                     recorded log and has no daemon to ask.";

/// One row the list drew, as the keys read it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Listed {
    /// The row's id: a session's, or a group's name. It is the key of the fold state.
    pub(crate) id: String,
    /// The session this row names, as an index into [`App::sessions`].
    pub(crate) session: Option<usize>,
    /// The number `/switch N` takes to reach this row.
    pub(crate) number: Option<usize>,
    /// What Enter does on it, performed by the head.
    pub(crate) enter: Option<RowAct>,
    /// Whether it folds, and which way it stands now.
    pub(crate) open: Option<Fold>,
}

/// The list, as drawn: the rows the keys act on, and the lines already rendered.
#[derive(Debug, Clone, Default)]
pub(crate) struct List {
    pub(crate) rows: Vec<Listed>,
    pub(crate) content: PaneLines,
}

impl List {
    /// The row the cursor is on, or `None` for a list with no stops in it.
    pub(crate) fn selected(&self, sel: usize) -> Option<&Listed> {
        if self.rows.is_empty() {
            return None;
        }
        self.rows.get(sel.min(self.rows.len() - 1))
    }
}

/// The head's stack for its panes, built once and kept: discovery reads the disk, and a list
/// drawn every frame cannot afford it. One stack serves every hook - the cache, the state and the
/// acts are keyed by hook, because two panes sharing one `state` would swap answers.
pub(crate) struct Host {
    stack: Stack,
    /// The last answer, by the input that asked for it. The list is drawn once per frame and read
    /// several times within it (the bar, the click map, the keys), so this is the difference
    /// between a pane that costs microseconds and one that costs a compile.
    cache: Mutex<Cached>,
    /// What each hook's layer answered with last time, handed back to it (state
    /// belongs to the host). Keyed by hook, so the queue pane's answer and the session list's
    /// cannot overwrite each other.
    state: Mutex<std::collections::BTreeMap<String, Value>>,
    /// The last failure a layer met, until somebody is told. Fail down LOUDLY
    /// (`docs/plugins-plan.md`): the list the person sees may be the built-in's or
    /// the head's own, and either way the reason their layer is not drawing it is news.
    problem: Mutex<Option<String>>,
    /// The folder this host was built for; a head that moves to another one gets another stack.
    workspace: PathBuf,
    /// When this host next looks for a rebuilt layer, on the head's clock. The daemon's
    /// `plugin load` cannot refresh a pane the HEAD draws, so the head watches its own scopes with
    /// `Stack::poll_changes` and a rebuild redraws with no restart. See `Host::reload_if_due`.
    next_poll: u64,
}

/// How often the head looks for a rebuilt layer - the daemon's own `POLL_EVERY`, for the same
/// reason: a stat per layer is free, and two seconds is the longest a person waits after saving.
const POLL_MS: u64 = 2_000;

#[derive(Default)]
struct Cached {
    /// `"<hook>\n<the input's JSON>"` - the hook is part of the key because one host answers two
    /// panes, and two panes' inputs can be byte-identical while their answers are not.
    key: String,
    answer: Option<Value>,
}

/// What one call came back with. The failure a layer met is not here: [`Host::ask`] keeps it for
/// [`App::take_sessions_failure`], because the one that has to say it is a caller with a screen.
pub(crate) struct Answered {
    pub(crate) view: Option<pv::View>,
    /// The answer as the layer wrote it: a `command_` or `complete_` hook's own shape, which is
    /// not a view.
    pub(crate) answer: Option<Value>,
    /// The fuel the answering layer's module burned on this call - `None` when nothing answered,
    /// or when the answer came from the cache and nothing was measured.
    pub(crate) fuel: Option<u64>,
}

impl Host {
    /// One `command_` hook through this host's stack, so the layer approved for the pane is the
    /// one that answers the command; the hook is built from the verb (`todo` -> `command_todo`).
    /// The answer is raw - the CALLER validates an `act` - and a failure is kept for its screen.
    pub(crate) fn command(
        &self,
        verb: &str,
        input: &serde_json::Value,
    ) -> letibot_plugins::Resolved {
        let hook = format!("command_{verb}");
        let resolved = self.stack.call(&hook, input);
        if let (Some(said), Ok(mut slot)) = (resolved.failures.first(), self.problem.lock()) {
            *slot = Some(format!(
                "a plugin layer could not answer `/{verb}`, so it was answered from below it: \
                 {said}"
            ));
        }
        resolved
    }

    /// The failure the last call recorded, without taking it - the queue pane's refusal sentence
    /// reads it on every draw, and taking it would eat the notice the picker owes its reader.
    pub(crate) fn peek_problem(&self) -> Option<String> {
        self.problem.lock().ok().and_then(|p| p.clone())
    }

    /// The failure the last call recorded, taken - the seam [`App::take_sessions_failure`] reads
    /// through, and the one a test uses to see that a broken layer was SEEN rather than skipped.
    #[cfg(test)]
    fn take_problem(&self) -> Option<String> {
        self.problem.lock().ok().and_then(|mut p| p.take())
    }

    /// A host with exactly this stack - what a test stands a broken one in with. The stack's own
    /// refusals come with it, because a layer that would not load is news the person is owed and
    /// this host is the last place that can say it.
    #[cfg(test)]
    pub(crate) fn with_stack(stack: Stack, workspace: PathBuf) -> Host {
        let problem = would_not_load(&stack);
        Host {
            stack,
            cache: Mutex::new(Cached::default()),
            state: Mutex::new(std::collections::BTreeMap::new()),
            problem: Mutex::new(problem),
            workspace,
            next_poll: 0,
        }
    }

    /// A host for this session's folder, with the binary's own layers at the bottom. One door for
    /// the head, and crate-visible because a test stands the real build in for a folder it cannot
    /// create.
    pub(crate) fn build(session: &str, workspace: &Path) -> Host {
        let mut stack = Stack::discover(
            session_plugins_dir(session).as_deref(),
            Some(workspace),
            user_config_dir().as_deref(),
            Arc::new(RanoLoader),
        );
        // The trust book, and the head is where a pin is TAKEN: the daemon's stack reads the same
        // file at its next poll, so one answer is in force in both processes. `set_trust`
        // re-decides every placed layer, and a pin whose bytes moved is SAID once.
        let mut broke: Vec<String> = stack
            .set_trust(letibot_plugins::Trust::open_default())
            .into_iter()
            .map(|b| {
                format!(
                    "the plugin layer `{}` is not pinned any more: its bytes changed after it was \
                     pinned — what was pinned is module sha256 {}, and on disk now {}; \
                     `/plugin trust {}` pins what is there now",
                    b.name, b.pin.module, b.module, b.name
                )
            })
            .collect();
        // The built-in scope at the bottom of the stack, and a failure here is SAYABLE: the head
        // still falls back to its own list, but a binary whose own layer is broken is a defect and
        // the person is owed the sentence. Recorded once, at build time - it is never asked.
        if let Some(builtin) = install_builtins(&mut stack) {
            broke.push(builtin);
        }
        if let Some(refused) = would_not_load(&stack) {
            broke.push(refused);
        }
        let problem = (!broke.is_empty()).then(|| broke.join("; "));
        Host {
            stack,
            cache: Mutex::new(Cached::default()),
            state: Mutex::new(std::collections::BTreeMap::new()),
            problem: Mutex::new(problem),
            workspace: workspace.to_path_buf(),
            next_poll: 0,
        }
    }

    /// Pick up a layer that was rebuilt on disk, at most every `POLL_MS` - the live-iteration loop
    /// for a pane the head draws. No shadow gate: the head gates nothing it discovers, and the
    /// gate a change must pass is the daemon's `plugin load`. The cache goes with the reload.
    fn reload_if_due(&mut self, now_ms: u64) {
        if now_ms != 0 && now_ms < self.next_poll {
            return;
        }
        self.next_poll = now_ms.saturating_add(POLL_MS);
        // The book before the modules: a pin taken by another process (or one whose bytes
        // moved) decides a layer without any module having changed, and a pane drawn from a layer
        // that stopped being pinned should say so on this head's own clock rather than a daemon's.
        self.stack.refresh_trust();
        let candidates = self.stack.poll_changes();
        if candidates.is_empty() {
            return;
        }
        let mut applied = false;
        for cand in candidates {
            if self.stack.apply(cand).is_ok() {
                applied = true;
            }
        }
        if applied {
            if let Ok(mut cache) = self.cache.lock() {
                cache.key.clear();
                cache.answer = None;
            }
            // A rebuilt layer is a NEW layer answering: whatever `state` the old one left behind
            // was its own, and handing it to a different module is handing it an answer to a
            // question it never asked.
            if let Ok(mut state) = self.state.lock() {
                state.clear();
            }
        }
    }

    /// The layers a person could pin, with what the book says about each - `/plugin`'s listing and
    /// the answer `/plugin trust NAME` reports. A built-in is not in it: a list of things that need
    /// no decision hides the ones that do.
    pub(crate) fn placed(&self) -> Vec<letibot_plugins::Layer> {
        self.stack.placed().into_iter().cloned().collect()
    }

    /// Pin a layer's bytes as they are now. The pin is taken through the head's own book, so the
    /// file is written once and the daemon reads it, and this stack is re-decided immediately so
    /// the pane does not wait for a poll to agree.
    pub(crate) fn pin(&mut self, name: &str) -> Result<(letibot_plugins::Pin, String), String> {
        let layer = self.stack.layer_named(name).cloned().ok_or_else(|| {
            let names: Vec<String> = self
                .stack
                .placed()
                .iter()
                .map(|l| l.manifest.name.clone())
                .collect();
            format!(
                "no placed layer `{name}` — this session's stack has [{}]",
                names.join(", ")
            )
        })?;
        if layer.scope == letibot_plugins::Scope::BuiltIn {
            return Err(format!(
                "`{name}` is a built-in — it is part of this binary, and D17 runs the built-ins \
                 freely: there is nothing to pin"
            ));
        }
        let trust = self.stack.trust().cloned().ok_or_else(|| {
            "this head has no trust book open, so nothing could be recorded".to_string()
        })?;
        let pin = trust.pin(&layer.manifest.name, &layer.dir)?;
        self.stack.refresh_trust();
        // Who wrote them, when a tool did - the third arm, reported beside the hash so the
        // person pinning knows whether these bytes are the model's or their own.
        let wrote = trust
            .writes_for(&layer.dir)
            .into_iter()
            .map(|w| format!("{} (session {})", w.path, w.session))
            .collect::<Vec<_>>();
        Ok((pin, wrote.join("; ")))
    }

    /// Take a pin back - `/plugin untrust NAME`.
    pub(crate) fn unpin(&mut self, name: &str) -> Result<letibot_plugins::Pin, String> {
        let trust = self
            .stack
            .trust()
            .cloned()
            .ok_or_else(|| "this head has no trust book open".to_string())?;
        let pin = trust
            .unpin(name)
            .or_else(|| {
                self.stack
                    .layer_named(name)
                    .and_then(|l| trust.unpin(&l.dir.display().to_string()))
            })
            .ok_or_else(|| format!("nothing is pinned as `{name}`"))?;
        self.stack.refresh_trust();
        Ok(pin)
    }

    /// The layer a pane's hook would ask. A head with a `queue` layer that does not read the queue
    /// feed is not that pane's: it may answer, but it would draw rows from data it never asked
    /// for, so the hook and the feed are one predicate.
    pub(crate) fn reader(&self, hook: &str, feed: Feed) -> Option<&letibot_plugins::Layer> {
        self.stack.layers().iter().find(|l| {
            !l.disabled
                && l.manifest.contract == letibot_plugins::CONTRACT
                && l.manifest.hooks.iter().any(|h| h == hook)
                && l.manifest.feeds.contains(&feed)
        })
    }

    /// The acts the layer answering `hook` declared - what a view may ask the head to perform.
    pub(crate) fn declared(&self, hook: &str, feed: Feed) -> Vec<String> {
        self.reader(hook, feed)
            .map(|l| l.manifest.actions.clone())
            .unwrap_or_default()
    }

    /// Ask `hook`, one call per distinct (hook, input). The failure sentence names the pane the
    /// caller is drawing, because one host answers two: the wrong name sends the reader to the
    /// wrong place.
    pub(crate) fn ask(&self, hook: &str, pane: &str, input: &Input) -> Answered {
        let key = format!(
            "{hook}\n{}",
            serde_json::to_string(input).unwrap_or_default()
        );
        if let Ok(cache) = self.cache.lock()
            && cache.key == key
        {
            return Answered {
                view: cache
                    .answer
                    .as_ref()
                    .and_then(|a| serde_json::from_value(a.clone()).ok()),
                answer: cache.answer.clone(),
                // A cache hit measured nothing: a caller that wants the fuel asks [`Host::call`],
                // which is uncached for exactly that reason.
                fuel: None,
            };
        }
        let value = serde_json::to_value(input).unwrap_or(Value::Null);
        let resolved = self.stack.call(hook, &value);
        // Fail down, loudly: a layer that traps is skipped by the stack and
        // the one below answers; the sentence is kept for the person whose layer it was.
        let why = resolved.failures.first().map(|f| f.to_string());
        let view = resolved
            .answer
            .as_ref()
            .and_then(|a| serde_json::from_value::<pv::View>(a.clone()).ok());
        if let Ok(mut cache) = self.cache.lock() {
            cache.key = key;
            cache.answer = resolved.answer.clone();
        }
        if let (Some(said), Ok(mut slot)) = (why.as_ref(), self.problem.lock()) {
            *slot = Some(format!(
                "a plugin layer could not draw {pane}, so it is drawn from below it: {said}"
            ));
        }
        Answered {
            view,
            answer: resolved.answer.clone(),
            fuel: resolved.by.as_deref().and_then(|id| self.stack.fuel(id)),
        }
    }

    /// One hook call through this host's stack, UNCACHED, with the fuel the answering module burned.
    /// A pane whose feed changes at its own pace (the chat, asked once per generation) or a hook
    /// that answers a keystroke (`complete_`) would spend the one-slot cache above on every frame.
    pub(crate) fn call(&self, hook: &str, pane: &str, input: &Value) -> Answered {
        let resolved = self.stack.call(hook, input);
        let why = resolved.failures.first().map(|f| f.to_string());
        let view = resolved
            .answer
            .as_ref()
            .and_then(|a| serde_json::from_value::<pv::View>(a.clone()).ok());
        if let (Some(said), Ok(mut slot)) = (why.as_ref(), self.problem.lock()) {
            *slot = Some(format!(
                "a plugin layer could not draw {pane}, so it is drawn from below it: {said}"
            ));
        }
        Answered {
            view,
            fuel: resolved.by.as_deref().and_then(|id| self.stack.fuel(id)),
            answer: resolved.answer,
        }
    }

    /// The state a hook's layer answered with last time - per hook, because two panes share
    /// this host and handing one pane's `state` back to the other would be handing it an answer to
    /// a question it never asked.
    pub(crate) fn state_of(&self, hook: &str) -> Option<Value> {
        self.state.lock().ok().and_then(|s| s.get(hook).cloned())
    }

    /// Remember what a hook's layer answered with, for its next call.
    pub(crate) fn remember(&self, hook: &str, state: Option<Value>) {
        if let Ok(mut slot) = self.state.lock() {
            match state {
                Some(v) => {
                    slot.insert(hook.to_string(), v);
                }
                None => {
                    slot.remove(hook);
                }
            }
        }
    }
}

/// Put the binary's own layers at the bottom of `stack`, and answer one sentence naming the layer
/// and the reason when one will not go; `None` when the whole table installed. The sentence names
/// no particular question, because it now reaches the session list and a slash command both.
fn install_builtins(stack: &mut Stack) -> Option<String> {
    let failed = letibot_builtins::install(stack);
    if failed.is_empty() {
        return None;
    }
    Some(format!(
        "this binary's built-in plugin layer could not be loaded — {} — so whatever it answers is \
         answered by the head itself",
        failed.join("; ")
    ))
}

/// The layers that never got as far as a call, in one sentence. A manifest this host refuses
/// at load has no call to fail in, so the host that built the stack is the only one that can say
/// it. A built-in is left out: [`install_builtins`] already said that one.
fn would_not_load(stack: &Stack) -> Option<String> {
    let said: Vec<String> = stack
        .failures()
        .iter()
        .filter(|f| f.scope != letibot_plugins::Scope::BuiltIn)
        .map(|f| format!("the plugin layer {f}"))
        .collect();
    (!said.is_empty()).then(|| said.join("; "))
}

/// The session's own plugin directory, where a session-scope layer lives.
fn session_plugins_dir(session: &str) -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(
        data.join("letibot")
            .join("sessions")
            .join(session)
            .join("plugins"),
    )
}

/// `$XDG_CONFIG_HOME/letibot`, whose `plugins/` is the user scope.
fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("letibot"))
}

impl App {
    /// The head's plugin stack, built for this session's folder if it is not there yet - one door,
    /// so the picker and a slash command ask the SAME stack, and a layer installed is a layer
    /// asked. A head that moves to another folder gets another stack: layers belong to a project.
    pub(crate) fn plugin_host(&self) -> std::cell::Ref<'_, Option<Host>> {
        let workspace = self.workspace_path();
        {
            let mut slot = self.sessions_host.borrow_mut();
            let stale = slot
                .as_ref()
                .map(|h| h.workspace != workspace)
                .unwrap_or(true);
            if stale {
                *slot = Some(Host::build(&self.session_id, &workspace));
            } else if let Some(host) = slot.as_mut() {
                // A layer rebuilt on disk is a pane that redraws - the live loop, and the
                // reason this pane is a plugin at all. See [`Host::reload_if_due`].
                host.reload_if_due(self.now_ms);
            }
        }
        self.sessions_host.borrow()
    }

    /// The session list, from the plugin - `None` when there is no layer to ask, when its
    /// answer is not a list this head can draw, or when it could not be trusted enough to draw it.
    /// Each of those is the head's own list's cue.
    pub(crate) fn sessions_list(&self, w: usize) -> Option<List> {
        let slot = self.plugin_host();
        let host = slot.as_ref()?;
        host.reader(HOOK, Feed::Sessions)?;
        let declared = host.declared(HOOK, Feed::Sessions);
        let state = host.state_of(HOOK);
        let input = self.sessions_input(state);
        let answered = host.ask(HOOK, "the session list", &input);
        if let Some(answer) = answered.view.as_ref() {
            host.remember(HOOK, answer.state.clone());
        }
        let view = answered.view?;
        // The host's rules, and the acts the layer declared (C3).
        if view.validate_for(&declared).is_err() {
            return None;
        }
        let (rows, empty) = tree_of(&view, &self.picker_folds)?;
        let width = w.max(20);
        let list = TreeList {
            title: TITLE.to_string(),
            rows,
            selected: Some(self.picker_sel),
            empty,
            fold_column: true,
            // The full id sits at column 6, under the mark and the fold column.
            detail_column: 6,
            margin: 2,
            hint: HINT.iter().map(|h| (*h).to_string()).collect(),
            notes: vec![NOTE.to_string()],
        };
        Some(List {
            rows: listed(&view, self),
            content: list.content(width),
        })
    }

    /// The sentence a layer's failure owes the person, once, taken by whoever opens the list: a
    /// failure line in a list of sessions is a row nobody asked for. Three kinds arrive - a pane
    /// call, a `/todo`, and a built-in that would not load at all.
    pub(crate) fn take_sessions_failure(&mut self) -> Option<String> {
        let slot = self.sessions_host.get_mut().as_ref()?;
        slot.problem.lock().ok().and_then(|mut p| p.take())
    }

    /// The folder a session's layers are found under - the one it is wired to, or this process's
    /// own directory when the daemon has not said yet. Public in the crate because a test stands
    /// a host in for this one, and has to build it for the folder the head will ask about.
    pub(crate) fn workspace_path(&self) -> PathBuf {
        match self.sessions.first().map(|s| s.wiring.workspace.clone()) {
            Some(w) if !w.is_empty() => PathBuf::from(w),
            _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Take the fleet reader the binary supplies.
    pub fn set_fleet_reader(&mut self, reader: letibot_sessionlog::FleetReader) {
        self.fleet_reader = Some(reader);
    }

    /// Read the other daemons' packages in the background while the picker is open, never from a
    /// paint: a reading connects to sockets. One at a time, [`FLEET_REFRESH_MS`] apart.
    pub fn refresh_fleet(&mut self) {
        if let Some(rx) = &self.fleet_job {
            match rx.try_recv() {
                Ok(read) => {
                    self.fleet = read.rows;
                    self.fleet_unlisted = read.unlisted;
                    self.fleet_job = None;
                    self.fleet_read = self.now_ms;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.fleet_job = None;
                    self.fleet_read = self.now_ms;
                }
            }
            return;
        }
        let Some(reader) = self.fleet_reader.clone() else {
            return;
        };
        if !self.is_open(Surface::Picker)
            || self.now_ms.saturating_sub(self.fleet_read) < FLEET_REFRESH_MS
                && self.fleet_read != 0
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("fleet".into())
            .spawn(move || {
                let _ = tx.send(reader());
            })
            .is_ok()
        {
            self.fleet_job = Some(rx);
        } else {
            self.fleet_read = self.now_ms;
        }
    }

    /// What the pane is asked with - the one feed it declared, the fold state, and the size.
    fn sessions_input(&self, state: Option<Value>) -> Input {
        let mut feeds = BTreeMap::new();
        feeds.insert(Feed::Sessions.name().to_string(), self.sessions_feed());
        Input {
            v: letibot_plugin::pane::V,
            state,
            look: None,
            event: self.sessions_event.borrow().clone(),
            size: letibot_plugin::pane::Size {
                w: self.cfg.width as u32,
                h: 40,
            },
            feeds,
            folded: self.picker_folds.clone(),
            below: None,
            ..Input::default()
        }
    }

    /// The daemon's session list as the feed spells it, one object per session. `project` and
    /// `system` are computed HERE: they are facts about the whole list, and a guest that
    /// recomputed them would be a second opinion about where a row goes. The layer says what.
    pub(crate) fn sessions_feed(&self) -> Value {
        let place = self.placements();
        let projects = self.projects(&place);
        let now = letibot_sessionlog::registry::now_ms();
        let name_of = |key: &str| {
            projects
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.name.clone())
                .unwrap_or_default()
        };
        let sessions: Vec<Value> = self
            .sessions
            .iter()
            .enumerate()
            // A removed session is not in the feed at all, so the layer's rows, its counts and
            // its numbering agree with the head's own walk by construction: one predicate
            // (`App::removed_chain`), two drawings.
            .filter(|(i, _)| !self.removed_chain(*i))
            .map(|(i, s)| {
                json!({
                    "id": s.session_id,
                    "title": s.title,
                    "slug": s.slug,
                    "parent": s.parent_session_id,
                    // The origin's wire word - the same one the store holds.
                    "origin": s.origin.as_str(),
                    "live": s.live,
                    "running": s.status.running,
                    "heads": s.status.heads,
                    "items": s.status.items,
                    "stored": s.stored_items,
                    "model": s.wiring.model,
                    "workspace": s.wiring.workspace,
                    "waits_on": s.waits_on_operator,
                    "nags_paused": s.nags_paused.is_some(),
                    // When it last moved, so a pane can say *is that one stuck?* - the one fact
                    // a row of a session list is read for. Off the daemon's own brief, which reads
                    // the store's row (`SessionBrief::moved_ms`).
                    "moved_ms": s.moved_ms,
                    // Where it sits: the project's key and the name to draw, and the `system`
                    // group when it is one of the harness's own agents.
                    "project": place[i].project,
                    "project_name": name_of(&place[i].project),
                    // Which top-level group the project is drawn in, decided here for the same
                    // reason as `project`.
                    "bucket": self.bucket_of(&place, &place[i].project, now).word(),
                    "system": place[i].system.map(|k| k.title()),
                })
            })
            .collect();
        let fleet: Vec<Value> = self
            .fleet
            .iter()
            .map(|f| {
                json!({
                    "id": f.id,
                    "name": fleet_name(f),
                    "facts": fleet_facts(f),
                    "under": fleet_under(f, &self.home),
                    "running": f.state == letibot_sessionlog::FleetState::Running,
                })
            })
            .collect();
        json!({
            "sessions": sessions,
            "fleet": fleet,
            "unlisted": (self.fleet_unlisted > 0)
                .then(|| letibot_sessionlog::fleet::unlisted_words(self.fleet_unlisted)),
            "here": self.session_id,
            // The one home the head shortens against - the same value its own list uses, so
            // a plugin's rows and the head's rows cannot disagree about what `~` means.
            "home": self.home,
            // The host's clock: a guest compiled for wasm has no `SystemTime`, so an age is the
            // host's fact and the layer's arithmetic. The daemon's feed carries the same field, so
            // the two feeds are one shape rather than two.
            "now": letibot_sessionlog::registry::now_ms(),
        })
    }

    /// What Enter does on a row, in the head's words - `switch` and `fold` are the head's own
    /// verbs, so the bar reads them here rather than being told them.
    pub(crate) fn list_says(&self, row: &Listed) -> Option<&'static str> {
        match &row.enter {
            None => None,
            Some(RowAct::Switch { .. }) => Some("switches"),
            Some(RowAct::Event { .. }) => Some("acts on it"),
            // A queue verb is not this list's to perform. The queue pane's keys are the ones
            // that act on an entry, and a session list row carrying `approve` is a layer reaching
            // for a key this pane does not have - said, not silently done.
            Some(RowAct::Verb { .. }) => {
                Some("asks for a queue verb, which this list has no key for")
            }
            Some(RowAct::Fold { .. }) => match row.open {
                Some(Fold::Open) => Some("folds it away"),
                Some(Fold::Closed) => Some("unfolds it"),
                // A fixed row folds nothing, so it offers no key.
                Some(Fold::Fixed) | None => None,
            },
        }
    }

    /// Perform a row's act. The three verbs C3 gives a pane, and nothing else.
    pub(crate) fn perform(&mut self, id: &str, act: &RowAct) -> Option<Action> {
        match act {
            RowAct::Switch { session } => self.switch_to(session.clone()),
            RowAct::Fold { .. } => {
                // The new state is the opposite of what is DRAWN, not of what the fold map holds: a
                // node closed by DEFAULT has no entry until somebody opens one, so reading the map
                // alone turns the first click into a second "open". Drawn knows the two exceptions.
                let open = !matches!(self.drawn_open(id), Some(Fold::Open));
                self.set_row_fold(id, open);
                None
            }
            RowAct::Event { event } => {
                *self.sessions_event.borrow_mut() = Some(letibot_plugin::pane::Event {
                    item: id.to_string(),
                    action: event.clone(),
                });
                None
            }
            // Said rather than done: the queue's verbs belong to the queue's pane, whose
            // cursor names the entry its keys act on. A session list has no such cursor, so a
            // row here naming an entry is a row asking for a key that is not on this screen.
            RowAct::Verb { verb, .. } => {
                let said = format!(
                    "`{id}` asks for `{verb}`, which is the queue pane's verb — the session list \
                     has no key for it. Nothing was sent."
                );
                self.say(&said);
                None
            }
        }
    }

    /// The list's rows, for the keys - the plugin's when there is one, the head's own
    /// otherwise, as one shape.
    pub(crate) fn picker_rows(&self) -> Vec<Listed> {
        self.picker_list(self.picker_width()).rows
    }

    /// The list being drawn - the layer's answer when there is one, the head's own tree when
    /// there is not. One door, because the drawing, the keys, the click map and `/switch N` must
    /// all be reading the same list.
    pub(crate) fn picker_list(&self, w: usize) -> List {
        self.sessions_list(w)
            .unwrap_or_else(|| self.session_list(w))
    }

    /// The head's own list, drawn from [`App::session_rows`] - the way out when no layer
    /// answers, and the same rano primitive the layer's answer is drawn through, so the two
    /// pictures cannot come to differ.
    pub(crate) fn session_list(&self, w: usize) -> List {
        let rows = self.session_rows();
        let mut tree: Vec<TreeRow> = Vec::with_capacity(rows.len());
        let mut listed: Vec<Listed> = Vec::with_capacity(rows.len());
        let mut number = 0;
        let mut said_project = false;
        for r in &rows {
            let open = r.open.map(|o| if o { Fold::Open } else { Fold::Closed });
            match r.idx {
                Some(i) => {
                    // A number is the n-th SESSION, which is what the drawn list writes beside
                    // the row and what `/switch N` reads - one enumeration, here.
                    number += 1;
                    let id = self.sessions[i].session_id.clone();
                    tree.push(TreeRow {
                        id: Some(id.clone()),
                        depth: r.depth,
                        number: Some(number),
                        open,
                        cells: vec![TreeCell::role(
                            self.row_name(i),
                            if id == self.session_id {
                                rano::style::Role::Strong
                            } else {
                                rano::style::Role::Plain
                            },
                        )],
                        facts: self.session_row_facts(i),
                        detail: vec![self.row_under(i)],
                        detail_column: None,
                        actions: vec![TreeAction::new("enter", "switches")],
                        ..TreeRow::default()
                    });
                    listed.push(Listed {
                        id: id.clone(),
                        session: Some(i),
                        number: Some(number),
                        enter: Some(RowAct::switch(id)),
                        open,
                    });
                }
                None if matches!(r.node, crate::app::session::Node::FleetUnlisted { .. }) => {
                    tree.push(TreeRow {
                        id: Some(r.node.key().to_string()),
                        depth: r.depth,
                        number: None,
                        open: None,
                        cells: vec![TreeCell::role(
                            clean(&r.node.title()),
                            rano::style::Role::Faint,
                        )],
                        ..TreeRow::default()
                    });
                    listed.push(Listed {
                        id: r.node.key().to_string(),
                        session: None,
                        number: None,
                        enter: None,
                        open: None,
                    });
                }
                None if matches!(r.node, crate::app::session::Node::Fleet { .. }) => {
                    let Some(f) = self.fleet.iter().find(|f| f.id == r.node.key()) else {
                        continue;
                    };
                    tree.push(TreeRow {
                        id: Some(f.id.clone()),
                        depth: r.depth,
                        number: None,
                        open: None,
                        cells: vec![TreeCell::role(
                            clean(&fleet_name(f)),
                            rano::style::Role::Plain,
                        )],
                        facts: vec![TreeCell::role(
                            clean(&fleet_facts(f)),
                            if f.state == letibot_sessionlog::FleetState::Running {
                                rano::style::Role::Pending
                            } else {
                                rano::style::Role::Faint
                            },
                        )],
                        detail: vec![clean(&fleet_under(f, &self.home))],
                        detail_column: None,
                        actions: vec![TreeAction::new("enter", "shows the attach command")],
                        ..TreeRow::default()
                    });
                    listed.push(Listed {
                        id: f.id.clone(),
                        session: None,
                        number: None,
                        enter: Some(RowAct::switch(f.id.clone())),
                        open: None,
                    });
                }
                None => {
                    // The project legend is said on the first project line only.
                    let legend = match &r.node {
                        crate::app::session::Node::Project { .. } if !said_project => {
                            said_project = true;
                            crate::app::session::PROJECT_LEGEND
                        }
                        crate::app::session::Node::Project { .. } => "",
                        _ => r.node.legend(),
                    };
                    // A node's line is a stop with an act of its own - `enter` folds it - and
                    // it carries no number, because a number is somewhere to go.
                    let key = r.node.key().to_string();
                    let words = if r.open == Some(true) {
                        "folds it away"
                    } else {
                        "unfolds it"
                    };
                    tree.push(TreeRow {
                        id: Some(key.clone()),
                        depth: r.depth,
                        number: None,
                        open,
                        cells: vec![TreeCell::role(
                            clean(&r.node.title()),
                            rano::style::Role::Strong,
                        )],
                        facts: Vec::new(),
                        detail: if legend.is_empty() {
                            Vec::new()
                        } else {
                            vec![legend.to_string()]
                        },
                        detail_column: None,
                        actions: vec![TreeAction::new("enter", words)],
                        ..TreeRow::default()
                    });
                    listed.push(Listed {
                        id: key,
                        session: None,
                        number: None,
                        enter: Some(RowAct::fold()),
                        open,
                    });
                }
            }
        }
        let list = TreeList {
            title: TITLE.to_string(),
            rows: tree,
            selected: Some(self.picker_sel),
            empty: EMPTY.to_string(),
            fold_column: true,
            detail_column: 6,
            margin: 2,
            hint: HINT.iter().map(|h| (*h).to_string()).collect(),
            notes: vec![NOTE.to_string()],
        };
        List {
            rows: listed,
            content: list.content(w.max(20)),
        }
    }

    /// A session's name on its row - its slug when it has one, `WAITING ON YOU` when it is
    /// waiting, and paused nags when they are. The layer's `name_of` is this same sentence, held
    /// together by `the_heads_list_and_the_plugins_list_draw_the_same_rows`.
    fn row_name(&self, i: usize) -> String {
        let s = &self.sessions[i];
        let name = if s.title.is_empty() {
            letibot_sessionlog::registry::short_id(&s.session_id).to_string()
        } else {
            s.title.clone()
        };
        let name = if s.slug.trim().is_empty() {
            name
        } else {
            format!("{} · {name}", s.slug)
        };
        let name = match s.waits_on_operator {
            0 => name,
            1 => format!("{name} · WAITING ON YOU"),
            n => format!("{name} · WAITING ON YOU ({n})"),
        };
        let name = if s.nags_paused.is_some() {
            format!("{name} · nags paused")
        } else {
            name
        };
        clean(&name)
    }

    /// The right-hand column of a session's row: what it IS, in one run of facts.
    fn session_row_facts(&self, i: usize) -> Vec<TreeCell> {
        let s = &self.sessions[i];
        let mut facts: Vec<String> = Vec::new();
        if s.status.running {
            facts.push("generating".into());
        }
        if !s.live {
            facts.push("on disk".into());
        }
        let rows = if s.stored_items > 0 {
            s.stored_items as usize
        } else {
            s.status.items
        };
        if rows > 0 {
            facts.push(format!("{rows} rows"));
        }
        if s.status.heads > 0 {
            facts.push(format!(
                "{} head{}",
                s.status.heads,
                if s.status.heads == 1 { "" } else { "s" }
            ));
        }
        if !s.wiring.model.is_empty() {
            facts.push(s.wiring.model.clone());
        }
        if facts.is_empty() {
            return Vec::new();
        }
        vec![TreeCell::role(
            facts.join(" · "),
            if s.status.running {
                rano::style::Role::Pending
            } else {
                rano::style::Role::Faint
            },
        )]
    }

    /// The faint line under a session's row: the full id, and the workspace when there is one  -
    /// for a stored session the only thing on the row that says what the conversation was about.
    fn row_under(&self, i: usize) -> String {
        let s = &self.sessions[i];
        let under = if s.wiring.workspace.is_empty() {
            s.session_id.clone()
        } else {
            format!("{}  {}", s.session_id, self.tilde_path(&s.wiring.workspace))
        };
        clean(&under)
    }

    /// The width the list is measured at when it is not being drawn - the frame's own, so a
    /// key and the picture it moves are one list rather than two renderings of one.
    pub(crate) fn picker_width(&self) -> usize {
        self.cfg.width.max(20)
    }

    /// The row under the cursor, from whichever list is drawn (`picker_rows`).
    pub(crate) fn picker_row(&self) -> Option<Listed> {
        let rows = self.picker_rows();
        if rows.is_empty() {
            return None;
        }
        rows.get(self.picker_sel.min(rows.len() - 1)).cloned()
    }

    /// How a row of the drawn list stands - `Some(Fold::Open)`/`Closed` for a row that folds,
    /// `None` for one that does not. Read by [`App::perform`] so a fold flips what the screen says
    /// rather than what the map happens to hold.
    fn drawn_open(&self, id: &str) -> Option<Fold> {
        self.picker_rows()
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.open)
    }

    /// Open or close a row that folds - one door for Enter, the arrows and a click, so a fold is
    /// one act however it was asked for. It is ALSO written into `expanded`, because the head's
    /// own tree reads that one: the way out starts from the state the person left.
    pub(crate) fn set_row_fold(&mut self, id: &str, open: bool) {
        self.picker_folds.insert(id.to_string(), open);
        let at = self.expanded.iter().position(|e| e == id);
        match (open, at) {
            (true, None) => self.expanded.push(id.to_string()),
            (false, Some(at)) => {
                self.expanded.remove(at);
            }
            _ => {}
        }
    }
}

/// Milliseconds between two readings of the other daemons.
const FLEET_REFRESH_MS: u64 = 5_000;

/// A fleet row's name, facts and faint line. The feed carries these words so the plugin draws
/// the head's sentences and cannot differ.
fn fleet_name(f: &letibot_sessionlog::FleetRow) -> String {
    if f.title.is_empty() {
        letibot_sessionlog::registry::short_id(&f.id).to_string()
    } else {
        f.title.clone()
    }
}

fn fleet_facts(f: &letibot_sessionlog::FleetRow) -> String {
    let mut facts = vec![f.state.word()];
    if let Some(p) = &f.placement {
        facts.push(p.clone());
    }
    if !f.model.is_empty() {
        facts.push(f.model.clone());
    }
    facts.join(" · ")
}

fn fleet_under(f: &letibot_sessionlog::FleetRow, home: &str) -> String {
    let ws = match f.workspace.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => f.workspace.clone(),
    };
    format!("{}  {ws}", f.id)
}

/// The tree block a sessions view answers with, and its empty sentence.
fn tree_of(view: &pv::View, folds: &BTreeMap<String, bool>) -> Option<(Vec<TreeRow>, String)> {
    let tree = view.panels.iter().find_map(|p| {
        p.shown().iter().find_map(|b| match b {
            pv::Block::Tree(t) => Some(t),
            _ => None,
        })
    })?;
    let mut rows = Vec::new();
    for (depth, row) in pv::rows_deep(&tree.rows) {
        let id = row.id().to_string();
        // The host's fold state wins over the plugin's, which is what makes a fold instant: the
        // press changes this map and the next draw, and the layer remembers nothing. Three rows it
        // does not reach: a leaf, a `fixed` group, and one the map was never asked about.
        let open = match (row.open, folds.get(&id)) {
            (Some(pv::Fold::Fixed), _) => Some(Fold::Fixed),
            (Some(_), Some(true)) => Some(Fold::Open),
            (Some(_), Some(false)) => Some(Fold::Closed),
            (Some(f), None) => Some(fold_of(f)),
            (None, _) => None,
        };
        rows.push(TreeRow {
            id: (!id.is_empty()).then(|| id.clone()),
            depth: if row.children.is_empty() {
                row.depth
            } else {
                depth
            },
            number: row.number,
            open,
            role: row.role.map(role_of),
            cells: row.cells.iter().map(cell_of).collect(),
            facts: row.facts.iter().map(cell_of).collect(),
            detail: row.under.clone(),
            detail_column: None,
            actions: row
                .actions
                .iter()
                .filter_map(|a| {
                    a.letter()
                        .map(|c| TreeAction::new(c.to_string(), a.label.clone()))
                })
                .chain(row.enter.as_ref().and_then(|a| enter_action(a, open)))
                .collect(),
            children: Vec::new(),
        });
    }
    Some((rows, tree.empty.clone()))
}

/// The Enter action a row carries, in the words the head's own list uses - so a plugin's list and
/// the head's read alike.
fn enter_action(act: &RowAct, open: Option<Fold>) -> Option<TreeAction> {
    let words = match act {
        RowAct::Switch { .. } => "switches",
        RowAct::Event { .. } => "acts on it",
        // A row asking for a queue verb draws no Enter key in this list - see `list_says`.
        RowAct::Verb { .. } => return None,
        RowAct::Fold { .. } => match open {
            Some(Fold::Open) => "folds it away",
            Some(Fold::Closed) => "unfolds it",
            Some(Fold::Fixed) | None => return None,
        },
    };
    Some(TreeAction::new("enter", words))
}

/// Control characters out of a row's words: a title and a workspace are somebody's, and an escape
/// in one would be measured one way and drawn another. The layer spells this rule too, because a
/// guest cannot call the host; the equality test is what keeps the two in step.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn fold_of(f: pv::Fold) -> Fold {
    match f {
        pv::Fold::Open => Fold::Open,
        pv::Fold::Closed => Fold::Closed,
        pv::Fold::Fixed => Fold::Fixed,
    }
}

fn cell_of(c: &pv::Cell) -> TreeCell {
    match c.role {
        Some(r) => TreeCell::role(c.text.clone(), role_of(r)),
        None => TreeCell::new(c.text.clone()),
    }
}

fn role_of(r: pv::Role) -> rano::style::Role {
    use rano::style::Role as R;
    match r {
        pv::Role::Muted => R::Faint,
        pv::Role::Working => R::Pending,
        pv::Role::Attention => R::Attention,
        pv::Role::Success => R::Success,
        pv::Role::Failure => R::Failure,
        pv::Role::Plain => R::Plain,
        pv::Role::Strong => R::Strong,
        pv::Role::Faint => R::Faint,
        pv::Role::Pending => R::Pending,
        // P2 maps a hue to a theme colour that reads on dark and light; until then it is plain.
        pv::Role::Hue(_) => R::Plain,
    }
}

/// The rows as the keys read them: the list's stops, in drawing order.
fn listed(view: &pv::View, app: &App) -> Vec<Listed> {
    let mut out = Vec::new();
    let Some(tree) = view.panels.iter().find_map(|p| {
        p.shown().iter().find_map(|b| match b {
            pv::Block::Tree(t) => Some(t),
            _ => None,
        })
    }) else {
        return out;
    };
    for (_, row) in pv::rows_deep(&tree.rows) {
        let id = row.id().to_string();
        if id.is_empty() {
            continue;
        }
        let open = match (row.open, app.picker_folds.get(&id)) {
            (Some(pv::Fold::Fixed), _) => Some(Fold::Fixed),
            (Some(_), Some(true)) => Some(Fold::Open),
            (Some(_), Some(false)) => Some(Fold::Closed),
            (Some(f), None) => Some(fold_of(f)),
            (None, _) => None,
        };
        out.push(Listed {
            session: app.sessions.iter().position(|s| s.session_id == id),
            number: row.number,
            enter: row.enter.clone(),
            open,
            id,
        });
    }
    out
}

/// The one `Loader` in this binary: rano compiles the module, this answers a hook. The duplication
/// with the daemon's copy is deliberate - `letibot_plugins` stays free of a wasm engine, and each
/// binary that runs a layer brings its own rather than pulling a JIT into everything.
pub(crate) struct RanoLoader;

impl Loader for RanoLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        let plugin = rano::plugin::Plugin::load_hooks(
            wasm,
            rano::plugin::Capabilities::default(),
            letibot_plugins::HOOK_PREFIXES,
        )
        .map_err(|why| format!("{}: {why}", wasm.display()))?;
        Ok(Box::new(RanoModule { plugin }))
    }

    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        _caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        let plugin = rano::plugin::Plugin::from_bytes_hooks(
            bytes,
            PathBuf::from(format!("<built-in {name}>")),
            rano::plugin::Capabilities::default(),
            letibot_plugins::HOOK_PREFIXES,
        )
        .map_err(|why| format!("built-in `{name}`: {why}"))?;
        Ok(Box::new(RanoModule { plugin }))
    }
}

struct RanoModule {
    plugin: rano::plugin::Plugin,
}

impl Module for RanoModule {
    fn hooks(&self) -> Vec<String> {
        self.plugin.hooks().to_vec()
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        self.plugin.call(hook, input)
    }

    fn fuel(&self) -> u64 {
        self.plugin.last_fuel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in layer answers through rano's engine, in this process - the head's half of
    /// `docs/plugins-plan.md`: the same module the daemon carries, loaded by the same
    /// engine, asked by the head that draws it.
    #[test]
    fn the_builtin_sessions_layer_loads_and_answers_in_the_head() {
        let tmp =
            std::env::temp_dir().join(format!("letibot-head-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("a temp dir");
        let host = Host::build("s-none", &tmp);
        assert!(
            host.stack.failures().is_empty(),
            "nothing failed to load: {:?}",
            host.stack.failures()
        );
        let layer = host
            .reader(HOOK, Feed::Sessions)
            .expect("the built-in sessions layer is found");
        assert_eq!(layer.id, "b-sessions");
        let input = Input {
            v: letibot_plugin::pane::V,
            feeds: [(
                "sessions".to_string(),
                json!({"sessions": [{"id": "s-1", "title": "one"}], "here": "s-1", "home": "/home/x"}),
            )]
            .into_iter()
            .collect(),
            ..Input::default()
        };
        let answered = host.ask(HOOK, "the session list", &input);
        assert_eq!(
            host.take_problem(),
            None,
            "the built-in answered, and nothing failed"
        );
        let view = answered.view.expect("the built-in answered");
        assert_eq!(view.title, "sessions in this daemon");
        assert_eq!(
            host.take_problem(),
            None,
            "every built-in installed, so there is nothing to say"
        );
    }

    /// The built-in `todo` layer is installed and answers the head's command, which is the one a
    /// person actually types. `install_builtins` is under test as much as the module: the table is
    /// generated at build time, so a layer in the tree and not in the binary fails HERE, named.
    #[test]
    fn the_builtin_todo_layer_is_installed_and_answers_the_heads_command() {
        let tmp = std::env::temp_dir().join(format!("letibot-head-todo-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("a temp dir");
        let host = Host::build("s-none", &tmp);
        assert!(
            host.stack.failures().is_empty(),
            "nothing failed to load: {:?}",
            host.stack.failures()
        );
        let layer = host
            .stack
            .layers()
            .iter()
            .find(|l| l.id == "b-todo")
            .expect("the built-in `todo` layer is in the stack");
        assert!(
            layer.manifest.hooks.iter().any(|h| h == "command_todo"),
            "and it declares the hook the head asks: {:?}",
            layer.manifest.hooks
        );
        assert_eq!(
            layer.manifest.actions,
            vec!["add_todo".to_string()],
            "the one act the head performs, declared where a reviewer reads it"
        );
        assert!(
            layer.manifest.capabilities.is_empty(),
            "it writes nothing itself — the head is what writes"
        );

        let resolved = host.command(
            "todo",
            &json!({"line": "title:\"x\" body:\"y\"", "feeds": {}}),
        );
        assert_eq!(
            resolved.by.as_deref(),
            Some("b-todo"),
            "the answer is this binary's own layer: {:?}",
            resolved.failures
        );
        assert_eq!(
            resolved
                .answer
                .as_ref()
                .and_then(|a| a.pointer("/act/args/content"))
                .and_then(Value::as_str),
            Some("x — y"),
            "and it parsed the line"
        );
        assert_eq!(
            resolved
                .answer
                .as_ref()
                .and_then(|a| a.pointer("/act/name"))
                .and_then(Value::as_str),
            Some("add_todo")
        );
        assert_eq!(host.take_problem(), None, "and nothing was owed a sentence");
    }

    /// A built-in that will not load is SAID, not silently skipped: the head still falls back to
    /// its own list, but a binary whose own layer is broken is a defect. A build cannot ship an
    /// empty table - the build script stops with one sentence.
    #[test]
    fn a_builtin_that_cannot_load_is_a_sentence_and_not_a_silence() {
        struct Refuses;
        impl Loader for Refuses {
            fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
                Err(format!("no engine here: {}", wasm.display()))
            }
        }
        let mut stack = Stack::discover(None, None, None, Arc::new(Refuses));
        let said = install_builtins(&mut stack).expect("a refusal is a sentence");
        assert!(said.contains("built-in plugin layer"), "{said}");
        assert!(said.contains("sessions"), "the layer is named: {said}");
        assert!(
            said.contains("answered by the head itself"),
            "and the fallback is named: {said}"
        );
        // And a stack whose built-ins DID install says nothing at all.
        let mut good = Stack::discover(None, None, None, Arc::new(RanoLoader));
        assert_eq!(install_builtins(&mut good), None);
    }
}
