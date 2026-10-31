//! **The session list, drawn from a plugin** — `docs/plugins-plan.md` §7 step 10: the built-in
//! `sessions` layer decides the groups, the names, the counts and the keys, and the head draws
//! what it answers through rano's one tree list.
//!
//! # Why the head runs the layer
//!
//! Every other pane is answered by the daemon (`crates/harnessd/src/plugins.rs`) and reaches a head
//! as a `PluginPane` frame. This one cannot be: the session list is how a person gets OUT of a
//! broken session, and a picker that needs a round trip through the daemon is a picker that is
//! unavailable exactly when it is wanted. The head already holds every fact the list is drawn from
//! (the `Sessions` frame), and — through `rano`, which it links for its terminal, its syntax and its
//! widgets — it already has the engine. So the stack is built here, from the same scopes and the
//! same crates, and the layer answers in-process.
//!
//! # What the head still owns
//!
//! The **fold state**, keyed by row id: the layer says which rows fold and how they start, and the
//! person's own answer travels back in `Input::folded` on the next call. A row that cannot fold
//! (`Fold::Fixed`) carries no act on Enter at all, so nothing in the head has to know that `mine`
//! is the group which never closes.
//!
//! And the **acts**: `switch` goes through [`App::switch_to`], `fold` toggles [`App::picker_folds`],
//! `event` asks the pane again. A layer can reach no others, because `plugin.toml` declares the
//! verbs and `View::validate_for` refuses the rest.
//!
//! # The way out
//!
//! [`App::sessions_list`] answers `None` for a stack with no `sessions` layer, one whose module will
//! not load, one that traps or runs out of fuel, and one whose answer is not a list this head can
//! draw. The picker then draws the head's own tree ([`App::session_list`]) — the same rano
//! primitive, the same rows, the same shape — which is the escape hatch that must never depend on
//! this file.

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

/// **The list's frame, in the head's own words** — the tree the head draws without a layer
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

/// What the list says when the daemon holds nothing — the same sentence the built-in layer answers
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

/// **The list, as drawn**: the rows the keys act on, and the lines already rendered.
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

/// **The head's stack for its panes**, built once and kept: discovering layers reads the disk,
/// and a list drawn every frame cannot afford it.
///
/// **One stack, every hook.** The session list and the queue pane ask *different* hooks of the
/// same layers (`pane_sessions`, `pane_queue`), and they must: a layer the person approved for
/// one pane is a layer that is in force, and a second `Stack` would be a second answer to that
/// question — plus a second compile of every built-in module. So the host is keyed by HOOK
/// where a hook is not `pane_sessions`: the cache, the fold-free state and the declared acts are
/// all per hook, because two panes sharing one `state` would hand each other's answers back.
pub(crate) struct Host {
    stack: Stack,
    /// **The last answer, by the input that asked for it.** The list is drawn once per frame and
    /// read several times within it (the bar, the click map, the keys), so one wasm call per
    /// distinct input is the difference between a pane that costs microseconds and one that costs
    /// a compile.
    cache: Mutex<Cached>,
    /// What each hook's layer answered with last time, handed back to it (§1, principle 5: state
    /// belongs to the host). Keyed by hook, so the queue pane's answer and the session list's
    /// cannot overwrite each other.
    state: Mutex<std::collections::BTreeMap<String, Value>>,
    /// **The last failure a layer met, until somebody is told.** Fail down LOUDLY
    /// (`docs/plugins-plan.md` §1, principle 4): the list the person sees may be the built-in's or
    /// the head's own, and either way the reason their layer is not drawing it is news.
    problem: Mutex<Option<String>>,
    /// The folder this host was built for; a head that moves to another one gets another stack.
    workspace: PathBuf,
    /// **When this host next looks for a rebuilt layer**, on the head's clock.
    ///
    /// A pane the HEAD draws is a pane the daemon's `plugin load` cannot refresh: that applies the
    /// layer to the DAEMON's stack, and this one is not it. So the head watches its own scopes —
    /// the same `Stack::poll_changes` the daemon's hot-reload tick uses, a stat per layer — and a
    /// `plugin build` (or a `plugin load`, whose gate builds first) is a pane that redraws with no
    /// rebuild of either binary and no restart. See [`Host::reload_if_due`].
    next_poll: u64,
}

/// **How often the head looks for a rebuilt layer** — the daemon's own `POLL_EVERY`, for the same
/// reason: a stat per layer is free, and two seconds is the longest a person waits after saving.
const POLL_MS: u64 = 2_000;

#[derive(Default)]
struct Cached {
    /// `"<hook>\n<the input's JSON>"` — the hook is part of the key because one host answers two
    /// panes, and two panes' inputs can be byte-identical while their answers are not.
    key: String,
    answer: Option<Value>,
}

/// What one call came back with. The failure a layer met is not here: [`Host::ask`] keeps it for
/// [`App::take_sessions_failure`], because the one that has to say it is a caller with a screen.
pub(crate) struct Answered {
    pub(crate) view: Option<pv::View>,
}

impl Host {
    /// **One `command_` hook through this host's stack** — the seam a slash command asks a layer
    /// by, over the same stack the pane draws from: one stack per head, so a layer the person
    /// approved for the pane is the layer that answers their command.
    ///
    /// The hook name is built from the VERB (`todo` → `command_todo`), because the prefix is the
    /// whole of what makes a verb a layer's: a second command a layer owns is a second word here
    /// and no new mechanism, the same way a pane's next act is one more word in `actions`.
    ///
    /// The answer is the raw [`Resolved`](letibot_plugins::Resolved): what an `act` means is the
    /// CALLER's to validate, because the caller is what performs it. The head understands exactly
    /// one act name (`add_todo`) and ignores every other, which is the half of the trust story
    /// `plugin.toml`'s `actions` is the other end of.
    ///
    /// **A layer that failed is kept, once.** The stack already skipped it and asked the layer
    /// below (§1, principle 4: fail down, loudly), and `Resolved::failures` carries the sentence —
    /// but the caller that says it is the one with a screen, and by the time a command's answer is
    /// read the `Resolved` may already be dropped. So the first failure also lands in `problem`,
    /// where [`App::take_sessions_failure`] finds it, exactly as a pane's does.
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

    /// The failure the last call recorded, taken — the seam [`App::take_sessions_failure`] reads
    /// through, and the one a test uses to see that a broken layer was SEEN rather than skipped.
    #[cfg(test)]
    fn take_problem(&self) -> Option<String> {
        self.problem.lock().ok().and_then(|mut p| p.take())
    }

    /// **A host with exactly this stack** — what a test needs to stand in a broken one, and the
    /// shape `docs/plugins-plan.md` §7 step 10's last paragraph is about: past the built-in, the
    /// head draws its own list.
    #[cfg(test)]
    pub(crate) fn with_stack(stack: Stack, workspace: PathBuf) -> Host {
        Host {
            stack,
            cache: Mutex::new(Cached::default()),
            state: Mutex::new(std::collections::BTreeMap::new()),
            problem: Mutex::new(None),
            workspace,
            next_poll: 0,
        }
    }

    /// **A host for this session's folder, with the binary's own layers at the bottom.**
    ///
    /// One door for the head ([`App::plugin_host`]) and public in the crate because a test stands
    /// the real build in for a head whose folder it cannot create: `docs/plugins-plan.md` §7 step
    /// 10's built-in scope is only proved by the module this binary carries.
    pub(crate) fn build(session: &str, workspace: &Path) -> Host {
        let mut stack = Stack::discover(
            session_plugins_dir(session).as_deref(),
            Some(workspace),
            user_config_dir().as_deref(),
            Arc::new(RanoLoader),
        );
        // **D17's book, and the head is where a pin is TAKEN** — `/plugin trust NAME` writes it,
        // and the daemon's stack reads the same file at its next poll, so one person's answer is
        // in force in both processes. Discovery happens first and `set_trust` re-decides every
        // placed layer against it, so a layer pinned before this head started is free here too.
        // **And a pin whose bytes moved while this head was not running is SAID**, once: the same
        // rule the daemon's tick follows (`PluginHost::with_trust`), because the head has no
        // warning stream of its own and a layer that quietly stopped being yours is the state the
        // ladder exists to make impossible to miss.
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
        // **The built-in scope at the bottom of the stack, and a failure here is SAYABLE.**
        //
        // The head still draws its own list when a built-in will not load — that is the way out
        // (`docs/plugins-plan.md` §7 step 10's last paragraph) — but it must not be SILENT: a
        // binary whose own layer is broken is a defect, and the person opening the list is owed
        // the sentence. It is recorded once, at build time, because a layer that never loads is
        // never asked and so has no call to fail in.
        if let Some(builtin) = install_builtins(&mut stack) {
            broke.push(builtin);
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

    /// **Pick up a layer that was rebuilt on disk**, at most every [`POLL_MS`].
    ///
    /// This is the live-iteration loop for a pane the head draws, and it is the same mechanism the
    /// daemon's hot-reload tick uses (`PluginHost::gate_now` → `Stack::poll_changes` +
    /// `Stack::apply`), one step shorter: the head does not run the shadow gate, because the head
    /// has no `Builder` and — the reason this is not a hole — it does not gate the session-scope
    /// layers it DISCOVERS either (`Stack::discover` loads what is on disk at build time). A module
    /// the operator rebuilt is the same trust as a module the operator wrote before the head
    /// started; the gate is the daemon's `plugin load`, which is what a change should pass before
    /// anybody believes it.
    ///
    /// **The cache goes with it.** It is keyed by the input, and the point of a reload is that the
    /// same input now answers differently — keeping it would draw the old view until something
    /// else changed the input, which reads as a reload that did not happen.
    fn reload_if_due(&mut self, now_ms: u64) {
        if now_ms != 0 && now_ms < self.next_poll {
            return;
        }
        self.next_poll = now_ms.saturating_add(POLL_MS);
        // **The book before the modules**: a pin taken by another process (or one whose bytes
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

    /// **The layers a person could pin, with what the book says about each** — `/plugin`'s listing,
    /// and the answer `/plugin trust NAME` reports back.
    ///
    /// A built-in is not in it: it runs freely by definition, and a list of things that need no
    /// decision is a list that hides the ones that do.
    pub(crate) fn placed(&self) -> Vec<letibot_plugins::Layer> {
        self.stack.placed().into_iter().cloned().collect()
    }

    /// **Pin a layer's bytes as they are now** — the operator's one act in D17's ladder.
    ///
    /// The pin is taken through the head's own book (`Trust`), so the file is written once and
    /// the daemon reads it; and this stack is re-decided immediately, so the pane the person is
    /// looking at does not wait for a poll to agree.
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
        // **Who wrote them**, when a tool did — the third arm, reported beside the hash so the
        // person pinning knows whether these bytes are the model's or their own.
        let wrote = trust
            .writes_for(&layer.dir)
            .into_iter()
            .map(|w| format!("{} (session {})", w.path, w.session))
            .collect::<Vec<_>>();
        Ok((pin, wrote.join("; ")))
    }

    /// **Take a pin back** — `/plugin untrust NAME`.
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

    /// **Re-decide this stack against the book** — a pin taken elsewhere, and the pins whose
    /// bytes moved. The head has no daemon to be told by, so it asks on its own clock.
    pub(crate) fn refresh_trust(&mut self) {
        self.stack.refresh_trust();
    }

    /// **The layer a pane's hook would ask.** A head with a `queue` layer that does not read the
    /// queue feed is not that pane's: it may answer, but it would be drawing rows from data it
    /// never asked for.
    ///
    /// The feed is the declaration the manifest carries, so this is one predicate over both:
    /// the hook the pane is asked by and the feed it says it reads.
    pub(crate) fn reader(&self, hook: &str, feed: Feed) -> Option<&letibot_plugins::Layer> {
        self.stack.layers().iter().find(|l| {
            !l.disabled
                && l.manifest.contract == letibot_plugins::CONTRACT
                && l.manifest.hooks.iter().any(|h| h == hook)
                && l.manifest.feeds.contains(&feed)
        })
    }

    /// The acts the layer answering `hook` declared — what a view may ask the head to perform.
    pub(crate) fn declared(&self, hook: &str, feed: Feed) -> Vec<String> {
        self.reader(hook, feed)
            .map(|l| l.manifest.actions.clone())
            .unwrap_or_default()
    }

    /// **Ask `hook`**, one call per distinct (hook, input).
    ///
    /// The failure sentence names the pane the caller is drawing, because one host now answers
    /// two: *a plugin layer could not draw the session list* is the right sentence for the picker
    /// and the wrong one for a queue, and a person reading it is being told where to look.
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
            };
        }
        let value = serde_json::to_value(input).unwrap_or(Value::Null);
        let resolved = self.stack.call(hook, &value);
        // **Fail down, loudly** (§1, principle 4): a layer that traps is skipped by the stack and
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
        Answered { view }
    }

    /// **The state a hook's layer answered with last time** — per hook, because two panes share
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

/// **Put the binary's own layers at the bottom of `stack`, and say so when one will not go.**
///
/// Every failure is answered as one sentence naming the layer and the reason, or `None` when the
/// whole table installed. A build cannot produce an empty table (`crates/builtins/build.rs` stops
/// with a sentence rather than shipping one), so a failure here means a module the engine refused
/// at load — which a person has to be told, because what they will see otherwise is a list that
/// quietly stopped being the layer's.
///
/// **The sentence names no particular question**, because it now reaches two: the session list
/// (which the head then draws itself) and a slash command (`/todo`, which the head then answers in
/// its own words). It used to say *so the session list is drawn by the head itself*, which was true
/// while the picker was the only reader of this slot — and would have been a lie on the screen of
/// somebody typing `/todo` in a binary whose wasm is broken.
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
    /// **The head's plugin stack, built for this session's folder if it is not there yet** — one
    /// door, so the picker and a slash command ask the SAME stack.
    ///
    /// It was the picker's alone (`sessions_list` built it on the first draw), and that was the
    /// hole this package found: `/todo` in a head that had never opened the list found no host and
    /// answered in the head's own words, so the built-in `todo` layer was installed and never
    /// asked — a layer that answers only after somebody presses a key that has nothing to do with
    /// it. The command asks for the host now, and the cost is the one the picker already paid:
    /// discovery reads the disk and the modules compile, once per head per folder.
    ///
    /// A head that moves to another folder gets another stack (`Host::workspace` is what the
    /// staleness is measured against), because a project's layers belong to the project.
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
                // **A layer rebuilt on disk is a pane that redraws** — the live loop, and the
                // reason this pane is a plugin at all. See [`Host::reload_if_due`].
                host.reload_if_due(self.now_ms);
            }
        }
        self.sessions_host.borrow()
    }

    /// **The session list, from the plugin** — `None` when there is no layer to ask, when its
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

    /// **The sentence a layer's failure owes the person**, once — taken by whoever opens the list.
    ///
    /// Taken rather than drawn: the picker is a list, and a failure line in a list of sessions is a
    /// row nobody asked for. It goes where every other one-off sentence in this head goes. Three
    /// kinds arrive here and each says what it is: a layer that failed a pane call (and was skipped
    /// for the one below), a layer that failed a `/todo` (same, said at the command —
    /// `Host::command`), and a built-in that would not load at all (and was skipped for the head's
    /// own words) — see [`install_builtins`].
    pub(crate) fn take_sessions_failure(&mut self) -> Option<String> {
        let slot = self.sessions_host.get_mut().as_ref()?;
        slot.problem.lock().ok().and_then(|mut p| p.take())
    }

    /// The folder a session's layers are found under — the one it is wired to, or this process's
    /// own directory when the daemon has not said yet. Public in the crate because a test stands
    /// a host in for this one, and has to build it for the folder the head will ask about.
    pub(crate) fn workspace_path(&self) -> PathBuf {
        match self.sessions.first().map(|s| s.wiring.workspace.clone()) {
            Some(w) if !w.is_empty() => PathBuf::from(w),
            _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// **What the pane is asked with** — the one feed it declared, the fold state, and the size.
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
        }
    }

    /// The daemon's session list as the feed spells it — one object per session, every field the
    /// list's rows are made of.
    ///
    /// **The tree's two facts are computed HERE and not by the layer.** `project` and `system` are
    /// what a row's place in the tree is decided by ([`App::session_rows`]), and they are facts
    /// about the whole list — a project's name depends on the other projects, and a reviewer is
    /// known by the chain it hangs from. A guest that recomputed them would be a second opinion
    /// about where a session goes, which is the one thing the layer must not have: it decides what
    /// a row SAYS, the head decides where it sits.
    fn sessions_feed(&self) -> Value {
        let place = self.placements();
        let projects = self.projects(&place);
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
            .map(|(i, s)| {
                json!({
                    "id": s.session_id,
                    "title": s.title,
                    "slug": s.slug,
                    "parent": s.parent_session_id,
                    // The origin's wire word — the same one the store holds.
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
                    // **When it last moved**, so a pane can say *is that one stuck?* — the one fact
                    // a row of a session list is read for. Off the daemon's own brief, which reads
                    // the store's row (`SessionBrief::moved_ms`).
                    "moved_ms": s.moved_ms,
                    // **Where it sits**: the project's key and the name to draw, and the `system`
                    // group when it is one of the harness's own agents.
                    "project": place[i].project,
                    "project_name": name_of(&place[i].project),
                    "system": place[i].system.map(|k| k.title()),
                })
            })
            .collect();
        json!({
            "sessions": sessions,
            "here": self.session_id,
            // **The one home the head shortens against** — the same value its own list uses, so
            // a plugin's rows and the head's rows cannot disagree about what `~` means.
            "home": self.home,
            // **The host's clock** — a guest compiled for wasm has no `SystemTime`, so an age is
            // the host's fact and the layer's arithmetic (`docs/plugins-plan.md` §4.9). The
            // daemon's own feed carries the same field, and this one so that the two feeds are one
            // shape rather than two.
            "now": letibot_sessionlog::registry::now_ms(),
        })
    }

    /// **What Enter does on a row, in the head's words** — `switch` and `fold` are the head's own
    /// verbs, so the bar reads them here rather than being told them.
    pub(crate) fn list_says(&self, row: &Listed) -> Option<&'static str> {
        match &row.enter {
            None => None,
            Some(RowAct::Switch { .. }) => Some("switches"),
            Some(RowAct::Event { .. }) => Some("acts on it"),
            // **A queue verb is not this list's to perform.** The queue pane's keys are the ones
            // that act on an entry, and a session list row carrying `approve` is a layer reaching
            // for a key this pane does not have — said, not silently done.
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

    /// **Perform a row's act.** The three verbs C3 gives a pane, and nothing else.
    pub(crate) fn perform(&mut self, id: &str, act: &RowAct) -> Option<Action> {
        match act {
            RowAct::Switch { session } => self.switch_to(session.clone()),
            RowAct::Fold { .. } => {
                // **The new state is the opposite of what is DRAWN**, not of what the fold map
                // holds: a project and its `system` branch open by default and have no entry
                // until somebody folds one, so reading the map alone turned the first click into
                // a second "open".
                let open = !matches!(self.drawn_open(id), Some(Fold::Open));
                self.set_row_fold(id, open);
                None
            }
            RowAct::Event { event } => {
                *self.sessions_event.borrow_mut() = Some(letibot_plugin::pane::Event {
                    item: id.to_string(),
                    action: event.clone(),
                });
                self.redraw = true;
                None
            }
            // **Said rather than done**: the queue's verbs belong to the queue's pane, whose
            // cursor names the entry its keys act on. A session list has no such cursor, so a
            // row here naming an entry is a row asking for a key that is not on this screen.
            RowAct::Verb { verb, .. } => {
                let said = format!(
                    "`{id}` asks for `{verb}`, which is the queue pane's verb — the session list \
                     has no key for it. Nothing was sent."
                );
                self.say(&said);
                self.redraw = true;
                None
            }
        }
    }

    /// **The list's rows, for the keys** — the plugin's when there is one, the head's own
    /// otherwise, as one shape.
    pub(crate) fn picker_rows(&self) -> Vec<Listed> {
        self.picker_list(self.picker_width()).rows
    }

    /// **The list being drawn** — the layer's answer when there is one, the head's own tree when
    /// there is not. One door, because the drawing, the keys, the click map and `/switch N` must
    /// all be reading the same list.
    pub(crate) fn picker_list(&self, w: usize) -> List {
        self.sessions_list(w)
            .unwrap_or_else(|| self.session_list(w))
    }

    /// **The head's own list, drawn from [`App::session_rows`]** — the way out when no layer
    /// answers, and the same rano primitive the layer's answer is drawn through, so the two
    /// pictures cannot come to differ.
    pub(crate) fn session_list(&self, w: usize) -> List {
        let rows = self.session_rows();
        let mut tree: Vec<TreeRow> = Vec::with_capacity(rows.len());
        let mut listed: Vec<Listed> = Vec::with_capacity(rows.len());
        let mut number = 0;
        for r in &rows {
            let open = r.open.map(|o| if o { Fold::Open } else { Fold::Closed });
            match r.idx {
                Some(i) => {
                    // **A number is the n-th SESSION**, which is what the drawn list writes beside
                    // the row and what `/switch N` reads — one enumeration, here.
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
                None => {
                    // **A node's line is a stop with an act of its own** — `enter` folds it — and
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
                        detail: vec![r.node.legend().to_string()],
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

    /// **A session's name on its row** — its slug when it has one, `WAITING ON YOU` when it is
    /// waiting, and paused nags when they are. The layer's `name_of` is this same sentence, and
    /// the two are held together by `the_heads_list_and_the_plugins_list_draw_the_same_rows`.
    ///
    /// **A title is somebody's text, so its control characters are drawn as spaces** — see
    /// [`clean`].
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

    /// The faint line under a session's row: the full id, and the workspace when there is one —
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

    /// **The width the list is measured at when it is not being drawn** — the frame's own, so a
    /// key and the picture it moves are one list rather than two renderings of one.
    pub(crate) fn picker_width(&self) -> usize {
        self.cfg.width.max(20)
    }

    /// **The row under the cursor, from whichever list is drawn** (`picker_rows`).
    pub(crate) fn picker_row(&self) -> Option<Listed> {
        let rows = self.picker_rows();
        if rows.is_empty() {
            return None;
        }
        rows.get(self.picker_sel.min(rows.len() - 1)).cloned()
    }

    /// **How a row of the drawn list stands** — `Some(Fold::Open)`/`Closed` for a row that folds,
    /// `None` for one that does not. Read by [`App::perform`] so a fold flips what the screen says
    /// rather than what the map happens to hold.
    fn drawn_open(&self, id: &str) -> Option<Fold> {
        self.picker_rows()
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.open)
    }

    /// **Open or close a row that folds** — one door for Enter, the arrows and a click, so a fold
    /// is one act however it was asked for.
    ///
    /// The state is the head's (`picker_folds`, keyed by row id) for the list that is DRAWN, and
    /// it is ALSO written into `expanded` — the head's own tree reads that one, so the way out (a
    /// head whose layer is broken) starts from the state the person left rather than from the
    /// default.
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
        self.redraw = true;
    }
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
        // **The host's fold state wins over the plugin's** — which is what makes a fold instant:
        // pressing it changes this map and the next draw, and the layer never remembers anything.
        //
        // **Three rows it does not reach**: one that said nothing about folding (a leaf), one that
        // said `fixed` (a group that cannot fold — a glyph there is the lie `Fold` exists to make
        // unsayable), and one the map has never been asked about.
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

/// The Enter action a row carries, in the words the head's own list uses — so a plugin's list and
/// the head's read alike.
fn enter_action(act: &RowAct, open: Option<Fold>) -> Option<TreeAction> {
    let words = match act {
        RowAct::Switch { .. } => "switches",
        RowAct::Event { .. } => "acts on it",
        // A row asking for a queue verb draws no Enter key in this list — see `list_says`.
        RowAct::Verb { .. } => return None,
        RowAct::Fold { .. } => match open {
            Some(Fold::Open) => "folds it away",
            Some(Fold::Closed) => "unfolds it",
            Some(Fold::Fixed) | None => return None,
        },
    };
    Some(TreeAction::new("enter", words))
}

/// **Control characters out of a row's words**: a title and a workspace are somebody's, and an
/// escape in one would be measured one way and drawn another.
///
/// **This is the sessions layer's own rule, spelled twice on purpose.** The layer is a GUEST — a
/// `cdylib` compiled for wasm with no dependency of the host's — so it cannot call this function,
/// and the host cannot call its. Two spellings of one rule are what the equality test
/// (`the_heads_list_and_the_plugins_list_draw_the_same_rows`) exists to keep in step: a title with
/// an escape in it drew a space from one list and the raw byte from the other.
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

/// **The one `Loader` in this binary**: rano compiles the module, this answers a hook.
///
/// The daemon's copy is `crates/harnessd/src/plugins/rano.rs`, and the duplication is deliberate:
/// `letibot_plugins` talks to a module through a trait so it can stay free of a wasm engine, and
/// the two binaries that run a layer — the daemon, and now the head, for the one pane that must not
/// depend on the daemon — each bring their own rather than making a third crate that pulls a JIT
/// into everything that links it.
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

    /// **The built-in layer answers through rano's engine, in this process** — the head's half of
    /// `docs/plugins-plan.md` §7 step 10: the same module the daemon carries, loaded by the same
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

    /// **The built-in `todo` layer is installed and answers the head's command** — the other half
    /// of §7 step 10, and the one a person actually types: `/todo title:"x" body:"y"` is a row
    /// because this binary carries a module that reads the line, and not because the head guessed.
    ///
    /// `install_builtins` is the function under test as much as the module is: the table is
    /// generated from `plugins/builtin/` at build time, so a layer that was added to the tree and
    /// not to the binary — or a manifest whose `hooks` and exports disagree, which
    /// `Manifest::admit` refuses — fails HERE, with the layer named, rather than as a command that
    /// quietly answers in the head's own words.
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

    /// **A built-in that will not load is SAID, not silently skipped.** The head still falls back
    /// to its own list — that is the way out — but a binary whose own layer is broken is a defect,
    /// and the sentence names the layer and the reason. (A build cannot ship an EMPTY table: the
    /// build script stops with one sentence, which its own run demonstrates.)
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
