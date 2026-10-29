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
//! draw. The picker then draws the head's own list (`App::session_picker`) — the same rano
//! primitive, the same rows — which is the escape hatch that must never depend on this file.

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

/// **The list's frame, in the picker's own words** — rano's `SessionPicker` draws exactly this, and
/// a plugin's list that looked different because a layer answered it would make *which layer drew
/// this* a guess.
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

/// **The head's stack for this pane**, built once and kept: discovering layers reads the disk, and
/// a list drawn every frame cannot afford it.
pub(crate) struct Host {
    stack: Stack,
    /// **The last answer, by the input that asked for it.** The list is drawn once per frame and
    /// read several times within it (the bar, the click map, the keys), so one wasm call per
    /// distinct input is the difference between a pane that costs microseconds and one that costs
    /// a compile.
    cache: Mutex<Cached>,
    /// What the layer answered with last time, handed back to it (§1, principle 5: state belongs
    /// to the host).
    state: Mutex<Option<Value>>,
    /// **The last failure a layer met, until somebody is told.** Fail down LOUDLY
    /// (`docs/plugins-plan.md` §1, principle 4): the list the person sees may be the built-in's or
    /// the head's own, and either way the reason their layer is not drawing it is news.
    problem: Mutex<Option<String>>,
    /// The folder this host was built for; a head that moves to another one gets another stack.
    workspace: PathBuf,
}

#[derive(Default)]
struct Cached {
    input: String,
    answer: Option<Value>,
}

/// What one call came back with. The failure a layer met is not here: [`Host::ask`] keeps it for
/// [`App::take_sessions_failure`], because the one that has to say it is a caller with a screen.
struct Answered {
    view: Option<pv::View>,
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
            state: Mutex::new(None),
            problem: Mutex::new(None),
            workspace,
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
        // **The built-in scope at the bottom of the stack, and a failure here is SAYABLE.**
        //
        // The head still draws its own list when a built-in will not load — that is the way out
        // (`docs/plugins-plan.md` §7 step 10's last paragraph) — but it must not be SILENT: a
        // binary whose own layer is broken is a defect, and the person opening the list is owed
        // the sentence. It is recorded once, at build time, because a layer that never loads is
        // never asked and so has no call to fail in.
        let problem = install_builtins(&mut stack);
        Host {
            stack,
            cache: Mutex::new(Cached::default()),
            state: Mutex::new(None),
            problem: Mutex::new(problem),
            workspace: workspace.to_path_buf(),
        }
    }

    /// **The layer this pane would ask.** A head with a `sessions` layer that does not read the
    /// sessions feed is not this pane's: it may answer, but it would be drawing a list from data it
    /// never asked for.
    fn reader(&self) -> Option<&letibot_plugins::Layer> {
        self.stack.layers().iter().find(|l| {
            !l.disabled
                && l.manifest.contract == letibot_plugins::CONTRACT
                && l.manifest.hooks.iter().any(|h| h == HOOK)
                && l.manifest.feeds.contains(&Feed::Sessions)
        })
    }

    /// The acts the answering layer declared — what a view may ask the head to perform.
    fn declared(&self) -> Vec<String> {
        self.reader()
            .map(|l| l.manifest.actions.clone())
            .unwrap_or_default()
    }

    /// **Ask the pane**, one call per distinct input.
    fn ask(&self, input: &Input) -> Answered {
        let text = serde_json::to_string(input).unwrap_or_default();
        if let Ok(cache) = self.cache.lock()
            && cache.input == text
        {
            return Answered {
                view: cache
                    .answer
                    .as_ref()
                    .and_then(|a| serde_json::from_value(a.clone()).ok()),
            };
        }
        let value = serde_json::to_value(input).unwrap_or(Value::Null);
        let resolved = self.stack.call(HOOK, &value);
        // **Fail down, loudly** (§1, principle 4): a layer that traps is skipped by the stack and
        // the one below answers; the sentence is kept for the person whose layer it was.
        let why = resolved.failures.first().map(|f| f.to_string());
        let view = resolved
            .answer
            .as_ref()
            .and_then(|a| serde_json::from_value::<pv::View>(a.clone()).ok());
        if let Ok(mut cache) = self.cache.lock() {
            cache.input = text;
            cache.answer = resolved.answer.clone();
        }
        if let (Some(said), Ok(mut slot)) = (why.as_ref(), self.problem.lock()) {
            *slot = Some(format!(
                "a plugin layer could not draw the session list, so it is drawn from below it: \
                 {said}"
            ));
        }
        Answered { view }
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
        host.reader()?;
        let declared = host.declared();
        let state = host.state.lock().ok().and_then(|s| s.clone());
        let input = self.sessions_input(state);
        let answered = host.ask(&input);
        if let Some(answer) = answered.view.as_ref()
            && let Ok(mut slot) = host.state.lock()
        {
            *slot = answer.state.clone();
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
    fn sessions_feed(&self) -> Value {
        let sessions: Vec<Value> = self
            .sessions
            .iter()
            .map(|s| {
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
                let open = !self.picker_folds.get(id).copied().unwrap_or(false);
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
        }
    }

    /// **The list's rows, for the keys** — the plugin's when there is one, the head's own
    /// otherwise, as one shape.
    pub(crate) fn picker_rows(&self) -> Vec<Listed> {
        if let Some(list) = self.sessions_list(self.picker_width()) {
            return list.rows;
        }
        let mut number = 0;
        self.session_rows()
            .iter()
            .map(|r| match r.idx {
                Some(i) => {
                    // **A number is the n-th SESSION**, which is what the drawn list writes beside
                    // the row and what `/switch N` reads — one enumeration, here.
                    number += 1;
                    Listed {
                        id: self.sessions[i].session_id.clone(),
                        session: Some(i),
                        number: Some(number),
                        enter: Some(RowAct::switch(self.sessions[i].session_id.clone())),
                        open: None,
                    }
                }
                None => Listed {
                    id: r.group.title().to_string(),
                    session: None,
                    number: None,
                    // **`mine` is the row Enter cannot act on** and it carries no act, so the bar
                    // cannot offer a fold that does not happen.
                    enter: (!r.group.is_mine()).then(RowAct::fold),
                    open: if r.group.is_mine() {
                        Some(Fold::Fixed)
                    } else if self.open_groups.contains(&r.group) {
                        Some(Fold::Open)
                    } else {
                        Some(Fold::Closed)
                    },
                },
            })
            .collect()
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

    /// **Open or close a row that folds** — one door for Enter, the arrows and a click, so a fold
    /// is one act however it was asked for.
    ///
    /// The state is the head's (`picker_folds`, keyed by row id), and it is ALSO written into the
    /// head's own lists — `open_groups` and `expanded` — so the way out (a head whose layer is
    /// broken) starts from the state the person left, rather than from the default.
    pub(crate) fn set_row_fold(&mut self, id: &str, open: bool) {
        self.picker_folds.insert(id.to_string(), open);
        if let Some(group) = SessionGroup::ALL.iter().find(|g| g.title() == id) {
            if !group.is_mine() {
                let at = self.open_groups.iter().position(|g| g == group);
                match (open, at) {
                    (true, None) => self.open_groups.push(*group),
                    (false, Some(at)) => {
                        self.open_groups.remove(at);
                    }
                    _ => {}
                }
            }
        } else {
            let at = self.expanded.iter().position(|e| e == id);
            match (open, at) {
                (true, None) => self.expanded.push(id.to_string()),
                (false, Some(at)) => {
                    self.expanded.remove(at);
                }
                _ => {}
            }
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
        RowAct::Fold { .. } => match open {
            Some(Fold::Open) => "folds it away",
            Some(Fold::Closed) => "unfolds it",
            Some(Fold::Fixed) | None => return None,
        },
    };
    Some(TreeAction::new("enter", words))
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
        let layer = host.reader().expect("the built-in sessions layer is found");
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
        let answered = host.ask(&input);
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
