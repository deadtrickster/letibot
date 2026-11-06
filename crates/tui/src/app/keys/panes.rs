//! What a surface's keys do: the dispatch from a key to the focused surface's binding
//! (`keys::bindings`), and each surface's acts - the outputs, the config pane, the overlays,
//! the session picker, and the subagents, todos, queue, standing and jobs panes.

use super::bindings::Act;
use super::*;
use crate::ui::panes::queue::{Selected, Verb, verb_moves};
use letibot_plugin::pane::Act as RowAct;
use std::ops::ControlFlow;

/// The next stop from `at` in a list of `n`, wrapping - the one rule every list's arrows keep.
fn step(at: usize, n: usize, up: bool) -> usize {
    match (up, at) {
        (true, 0) => n - 1,
        (true, _) => at - 1,
        (false, _) => (at + 1) % n,
    }
}

/// Which way a key moves: up for ↑, PageUp and a notch up.
fn upward(k: &Key) -> bool {
    matches!(k, Key::Up | Key::PageUp | Key::WheelUp | Key::Left)
}

impl App {
    /// The keys of the surface in front, and of no other: the stack's top, which is the same value
    /// `main_area` draws. The key is looked up in that surface's table; a key it does not name falls
    /// through to the shared rules: Esc and ctrl-c pop, and Enter is owned (`Surface::owns_enter`).
    pub(crate) fn key_surface(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        let Some(s) = self.focused_surface() else {
            return ControlFlow::Continue(());
        };
        if let Some(b) = self.binding_for(s, k)
            && let ControlFlow::Break(r) = self.surface_act(s, b.act, k)
        {
            return ControlFlow::Break(r);
        }
        match k {
            Key::Esc | Key::CtrlC => {
                // A plugin pane's Esc leaves the VIEW before it leaves the pane (C3's ladder:
                // detail, then view, then the pane). The detail is a surface, so the pop below is
                // already that step; a view is not a surface, so this one is the pane's own.
                if self.plugin_esc() {
                    return ControlFlow::Break(None);
                }
                // A plugin's pane leaving the screen is the daemon's business too (`PaneClose`):
                // the host keeps a refresh clock per open pane, and a dashboard nobody is looking
                // at must stop reading files. The pop below is silent, hence a step of its own.
                if self.focused_surface() == Some(Surface::PluginPane) {
                    return ControlFlow::Break(self.close_plugin());
                }
                self.pop_surface();
                ControlFlow::Break(None)
            }
            // Swallowed, and said: with a line typed, Enter over a reading pane did nothing.
            Key::Enter if s.owns_enter() => {
                if !self.editor.text().is_empty() {
                    self.say("this pane takes enter — your line is held; esc closes the pane, then enter sends it");
                }
                ControlFlow::Break(None)
            }
            _ => ControlFlow::Continue(()),
        }
    }

    /// One act, on one surface. `Continue` is the act declining - nothing to act on - and the
    /// key then falls through as though no binding had named it.
    pub(crate) fn surface_act(
        &mut self,
        s: Surface,
        act: Act,
        k: &Key,
    ) -> ControlFlow<Option<Action>> {
        // A key that is not the removal ends its arm: the second press is the confirmation. A
        // letter typed into the composer is unbound here and never reaches a binding, so it
        // cannot disarm anything - the arm's own clock ends that case (`removal::ARM_MS`).
        if !matches!(act, Act::Remove) {
            self.clear_removal_arm();
        }
        let up = upward(k);
        match act {
            Act::Swallow => ControlFlow::Break(None),
            // The window moves in the surface's own units: a line for an arrow, a page for a page
            // key, three lines a notch. `pane_scroll` clamps against the last draw's numbers,
            // because the key handler has no width or height and a guessed clamp walks past the end.
            Act::Scroll => {
                let by = match k {
                    Key::PageUp | Key::PageDown => self.screen_rows.max(1),
                    Key::WheelUp | Key::WheelDown => 3,
                    _ => 1,
                };
                self.scroll_pane(up, by);
                ControlFlow::Break(None)
            }
            Act::Cursor => self.cursor_act(s, up),
            Act::Roster => self.rooms_toggle_roster(),
            Act::DismissSend => self.rooms_dismiss_send(),
            Act::ResendSend => self.rooms_resend_send(),
            Act::Open => self.open_act(s),
            Act::Alt => self.todos_alt(),
            // `w` opens what the selected row waits on - the keyboard's half of the click on its
            // `waits on …` link (`App::open_wait`). A row that waits on nothing declines, and the
            // letter stays the composer's.
            Act::WaitsOn => match self.selected_wait() {
                Some(w) => ControlFlow::Break(self.open_wait(&w)),
                None => ControlFlow::Continue(()),
            },
            Act::Peek => self.subagents_peek(),
            Act::Hand => self.skill_hand(),
            // `o` goes into the reviewer's session: the row under the cursor, or the one opened
            // whole.
            Act::Reviewer => ControlFlow::Break(self.go_to_reviewer()),
            // `x` retires the monitor under the cursor; with none live the letter is the composer's.
            Act::Retire => match self.retire_selected_monitor() {
                Some(a) => ControlFlow::Break(Some(a)),
                None => ControlFlow::Continue(()),
            },
            // Enter reads the same subagent again, because a running one has new output.
            Act::Reread => match self.sub_out.as_ref() {
                Some(v) => {
                    let id = v.session_id.clone();
                    self.ask_peek(&id);
                    ControlFlow::Break(Some(Action::Peek(id)))
                }
                None => ControlFlow::Continue(()),
            },
            // A job's output pages: → and Enter ask for the page after the one on screen (or
            // re-read the end of a running job), ← walks back. `→` keeps the empty-composer guard
            // Enter does not have, because it is a cursor key first and a half-typed line moves.
            Act::PageOut => ControlFlow::Break(self.job_out_page(!matches!(k, Key::Left))),
            Act::Expand | Act::Collapse => self.picker_fold(act == Act::Expand),
            // `tab` moves between the queue's two views, and the strip the pane draws says so.
            // It works over an empty queue too: the strip still says which half you are looking at.
            Act::View => {
                self.queue_view = self.queue_view.other();
                ControlFlow::Break(None)
            }
            Act::Verb(v) => self.queue_verb(v),
            // A plugin's own keys, read from the view on the screen: a panel's key, `f`, `l`, or
            // an action letter of the selected stop.
            Act::PluginKey => self.plugin_key(k),
            // The pane the silence is seen from is where it is switched: the same verb typed, so
            // the key and `/nag` cannot drift, and the daemon's `nags_paused` / `nags_resumed`
            // row is what moves this head's resident line.
            Act::NagToggle => {
                if self.session_id.is_empty() {
                    return ControlFlow::Continue(());
                }
                let line = if self.nags_paused.is_some() {
                    "nag resume"
                } else {
                    "nag pause"
                };
                ControlFlow::Break(Some(Action::Slash { line: line.into() }))
            }
            // `d` folds the done group away, or opens it: the switch is this head's
            // ([`App::toggle_todos_fold_done`]), so nothing goes to the daemon and the pane draws
            // the answer on the next frame.
            Act::FoldDone => {
                self.toggle_todos_fold_done();
                ControlFlow::Break(None)
            }
            // `del` on the session list: arm, or remove. See `app::removal`.
            Act::Remove => {
                self.remove_selected();
                ControlFlow::Break(None)
            }
            // `ctrl-z`: the last removal, back.
            Act::Undo => {
                self.restore_removed();
                ControlFlow::Break(None)
            }
        }
    }

    /// Up/down on a list: the cursor moves through the list's ONE enumeration of stops - the same
    /// list the drawn `▸`, Enter and a click read - wrapping at the ends, with the window
    /// following it so the cursor is never walked out of the window. An empty list declines.
    fn cursor_act(&mut self, s: Surface, up: bool) -> ControlFlow<Option<Action>> {
        match s {
            Surface::Picker => {
                let n = self.picker_rows().len();
                if n == 0 {
                    return ControlFlow::Continue(());
                }
                self.picker_sel = step(self.picker_sel.min(n - 1), n, up);
            }
            // The view follows the config cursor, its `from …` line too (`config_follow`).
            Surface::Config => {
                let n = self.config_rows().len().max(1);
                self.config_sel = step(self.config_sel.min(n - 1), n, up);
                self.config_follow();
            }
            Surface::Todos => {
                let stops = self.todos_stops();
                if stops.is_empty() {
                    return ControlFlow::Continue(());
                }
                let n = stops.len();
                self.todos_sel = step(self.todos_sel.min(n - 1), n, up);
                self.sync_repo_from_stop(&stops);
                self.scroll_into_view(self.todos_row_of());
            }
            Surface::Queue => {
                let n = self.queue_stop_count();
                if n == 0 {
                    return ControlFlow::Continue(());
                }
                self.queue_sel = step(self.queue_sel.min(n - 1), n, up);
                self.scroll_into_view(self.queue_row_of());
            }
            Surface::Standing => {
                let n = self.standing.len();
                if n == 0 {
                    return ControlFlow::Continue(());
                }
                self.standing_sel = step(self.standing_sel.min(n - 1), n, up);
                self.scroll_into_view(self.standing_row_of());
            }
            Surface::Skills => {
                let n = self.skills.len();
                if n == 0 {
                    return ControlFlow::Continue(());
                }
                self.skills_sel = step(self.skills_sel.min(n - 1), n, up);
                self.scroll_into_view(self.skills_row_of());
            }
            Surface::Subagents => {
                let n = self.subagent_stops().len();
                if n == 0 {
                    return ControlFlow::Continue(());
                }
                self.subagents_sel = step(self.subagents_sel.min(n - 1), n, up);
                self.scroll_into_view(self.subagents_row_of());
            }
            Surface::Jobs => {
                let n = self.job_stops().len();
                if n == 0 {
                    return ControlFlow::Continue(());
                }
                self.jobs_sel = step(self.jobs_sel.min(n - 1), n, up);
                self.scroll_into_view(self.jobs_row_of());
            }
            Surface::Monitors => {
                let n = self.monitor_stops().len();
                if n == 0 {
                    return ControlFlow::Continue(());
                }
                self.monitors_sel = step(self.monitors_sel.min(n - 1), n, up);
                self.scroll_into_view(self.monitors_row_of());
            }
            // A plugin's pane: the cursor walks its STOPS - the lines and rows with an id, the
            // markdown items and the findings (C3) - and the window follows it.
            Surface::PluginPane => {
                let n = self.plugin_stops().len();
                if n == 0 {
                    return ControlFlow::Continue(());
                }
                self.plugin_sel = step(self.plugin_sel.min(n - 1), n, up);
                self.scroll_into_view(self.plugin_row_of());
            }
            Surface::Rooms => self.rooms_step(up),
            _ => return ControlFlow::Continue(()),
        }
        ControlFlow::Break(None)
    }

    /// Enter on a list: the row's act.
    pub(crate) fn open_act(&mut self, s: Surface) -> ControlFlow<Option<Action>> {
        match s {
            // The session under the cursor. The empty-composer rule is the binding's: a
            // half-typed id's Enter still means the id, and `submit` takes it.
            Surface::Picker => {
                // The row's own act, from whichever list is drawn: `switch` on a session, `fold`
                // on a group (`app::sessions_pane`). The layer decides which rows act and how;
                // the head performs it.
                let Some(row) = self.picker_row() else {
                    return ControlFlow::Continue(());
                };
                let Some(act) = row.enter.clone() else {
                    // A row with no act on Enter - there is none in this tree - must say nothing
                    // rather than look like a broken key.
                    return ControlFlow::Break(None);
                };
                ControlFlow::Break(self.perform(&row.id, &act))
            }
            // Changes the row under the cursor when it is one that can change now, and says why
            // when it is not.
            Surface::Config => ControlFlow::Break(self.config_change()),
            Surface::Todos => self.todos_read(),
            Surface::Queue => {
                // A row that folds is folded by Enter, before Enter means open the entry: the
                // history's own row says `enter opens it` and carries `{"fold": true}`, so the
                // finished rows cannot be reached at all without this.
                if self.queue_fold() {
                    return ControlFlow::Break(None);
                }
                // What Enter opens is the ENTRY, with the ask it was produced under, the state and
                // its reason, and the verdict's evidence. The id comes from the row that was DRAWN,
                // so a pane drawn by a layer opens the row being looked at.
                let Some(id) = self.queue_selected_id() else {
                    return ControlFlow::Continue(());
                };
                self.queue_open = Some(id);
                self.push_surface(Surface::QueueEntry);
                ControlFlow::Break(None)
            }
            Surface::Standing => {
                if self.standing.is_empty() {
                    return ControlFlow::Continue(());
                }
                self.open_note();
                ControlFlow::Break(None)
            }
            Surface::Skills => self.skill_open(),
            Surface::Subagents => self.subagents_enter(),
            Surface::Jobs => self.jobs_enter(),
            Surface::Monitors => {
                self.open_monitor_details();
                ControlFlow::Break(None)
            }
            // Enter reads the stop under the cursor whole: a finding with its evidence, its fix
            // and its verify; a row with its `detail` (C3).
            Surface::PluginPane => ControlFlow::Break(self.plugin_enter()),
            _ => ControlFlow::Continue(()),
        }
    }

    /// Right and left on the session picker: a row with rows under it opens and closes, the gesture
    /// a tree uses everywhere - and what lets a collapsed default afford to be collapsed. A child's
    /// left closes its PARENT, so left means back up the tree, not nothing on the row just reached.
    fn picker_fold(&mut self, open: bool) -> ControlFlow<Option<Action>> {
        // A row that folds is the row's own act, with this key's direction: the layer says which
        // rows fold (`enter: {fold: true}`), the head keeps the state, and the two arrows are the
        // gesture Enter toggles.
        let Some(row) = self.picker_row() else {
            return ControlFlow::Continue(());
        };
        match &row.enter {
            Some(RowAct::Fold { .. }) => {
                self.set_row_fold(&row.id, open);
                return ControlFlow::Break(None);
            }
            // A row with no act on Enter that also cannot fold says nothing rather than
            // appearing to swallow the key.
            None if row.session.is_none() => {
                return ControlFlow::Break(None);
            }
            _ => {}
        }
        // A session's left is the way back up the tree, which is what the arrows mean on a row
        // that cannot fold: close the parent when it is drawn on this same list, and otherwise
        // close the GROUP the row is drawn in - back out of the tree you are in, one level up.
        let Some(i) = row.session else {
            return ControlFlow::Break(None);
        };
        if open {
            let id = self.sessions[i].session_id.clone();
            self.set_row_fold(&id, true);
            return ControlFlow::Break(None);
        }
        let rows = self.picker_rows();
        let at = self.picker_sel.min(rows.len().saturating_sub(1));
        // The edge a row sits on is its parent WHEN the parent is drawn in the same group: that
        // is the rule the list was drawn by, and a parent in another group is not an edge here.
        // Then left closes the parent, otherwise the group the row is a root of.
        let under_same_group = self.sessions[i]
            .parent_session_id
            .clone()
            .and_then(|parent| {
                let p = rows.iter().position(|r| {
                    r.session
                        .is_some_and(|k| self.sessions[k].session_id == parent)
                })?;
                (group_at(&rows, p) == group_at(&rows, at)).then_some(parent)
            });
        match under_same_group {
            Some(parent) => self.set_row_fold(&parent, false),
            None => {
                if let Some(group) = group_at(&rows, at) {
                    let id = rows[group].id.clone();
                    self.set_row_fold(&id, false);
                }
            }
        }
        ControlFlow::Break(None)
    }

    /// Enter on the subagent tree IS the switch into that subagent's session: entering a row means
    /// the same thing everywhere in this head. The group row is the other thing Enter means here -
    /// Enter on it unfolds the finished children, the rule the todos pane keeps.
    fn subagents_enter(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.subagent_stops();
        let Some(stop) = stops.get(self.subagents_sel.min(stops.len().saturating_sub(1))) else {
            return ControlFlow::Continue(());
        };
        match *stop {
            SubStop::Finished => {
                self.subagents_finished_open = !self.subagents_finished_open;
                ControlFlow::Break(None)
            }
            SubStop::Agent(i) => {
                let row = &self.subagents[i];
                if row.state == "opening" {
                    // Nothing to attach to yet, and the daemon would refuse the switch by name
                    // anyway; saying it here keeps the operator in the pane they were using.
                    self.say("that subagent is still opening — nothing to attach to yet");
                    return ControlFlow::Break(None);
                }
                let id = row.session_id.clone();
                self.close_surface(Surface::Subagents);
                ControlFlow::Break(self.switch_to(id))
            }
        }
    }

    /// `p`: reading, not moving. The output opens on the `Peeked` reply and this head never leaves
    /// the session it is in - the whole argument for the `Peek` frame. Nothing to read under the
    /// fold header, so `p` on it falls through.
    fn subagents_peek(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.subagent_stops();
        let Some(SubStop::Agent(i)) = stops
            .get(self.subagents_sel.min(stops.len().saturating_sub(1)))
            .copied()
        else {
            return ControlFlow::Continue(());
        };
        let row = &self.subagents[i];
        if row.state == "opening" {
            self.say("that subagent is still opening — nothing to read yet");
            return ControlFlow::Break(None);
        }
        let id = row.session_id.clone();
        self.ask_peek(&id);
        ControlFlow::Break(Some(Action::Peek(id)))
    }

    /// Enter on the todos pane reads the row under the cursor whole, in an overlay - the one place a
    /// row longer than the pane can be read at all. The add control is the exception: there is no
    /// text there. With a line typed, the composer's Enter is the answer (`submit`).
    fn todos_read(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.todos_stops();
        let Some(stop) = stops.get(self.todos_sel.min(stops.len().saturating_sub(1))) else {
            return ControlFlow::Continue(());
        };
        match stop {
            TodoStop::Add => self.open_todo_card(),
            // A row waiting on YOU opens its question, the act the row is waiting for. The card
            // carries the row's words, so nothing the read overlay shows is lost.
            TodoStop::Mine(content) | TodoStop::Theirs(content)
                if self.operator_asks().iter().any(|a| &a.content == content) =>
            {
                let content = content.clone();
                self.open_operator_ask(Some(&content));
            }
            row => {
                self.todos_open = Some(row.clone());
                self.push_surface(Surface::TodoRow);
            }
        }
        ControlFlow::Break(None)
    }

    /// Tab is the todos pane's own acts: the card on the control, the toggle on one of the
    /// operator's rows, the unfold on a repo item. On the model's rows there is no act to perform,
    /// said rather than left as a key that does nothing. Half-typed, tab completes a `/command`.
    fn todos_alt(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.todos_stops();
        let Some(stop) = stops
            .get(self.todos_sel.min(stops.len().saturating_sub(1)))
            .cloned()
        else {
            return ControlFlow::Continue(());
        };
        // The stop the cursor is on, kept so a row that MOVES under the key is followed rather
        // than left behind - see the `Mine` arm below.
        let was = stop.clone();
        match stop {
            TodoStop::Add => {
                self.open_todo_card();
                ControlFlow::Break(None)
            }
            // Marking done is the operator's own row, though the status is the model's act too: the
            // daemon's `set_operator_states` is its own door and `/todo done N` the typed one. A
            // completed row reopens, because a cursor that only goes one way cannot be corrected.
            TodoStop::Mine(content) => {
                use letibot_sessionlog::event::TodoStatus;
                let mut mine = self.operator_todos();
                if let Some(t) = mine.iter_mut().find(|t| t.content == content) {
                    t.status = if t.status == TodoStatus::Completed {
                        TodoStatus::Pending
                    } else {
                        TodoStatus::Completed
                    };
                }
                self.say("toggled");
                self.echo_operator_todos(mine.clone());
                // The cursor follows the ROW, not the index it had: marking a row done moves it into
                // the done group, and an index kept across that lands on whichever row slid into the
                // hole. The next DRAW scrolls to where the row landed.
                let stops = self.todos_stops();
                if let Some(at) = stops.iter().position(|here| *here == was) {
                    self.todos_sel = at;
                }
                self.todos_follow = true;
                ControlFlow::Break(Some(Action::SetOperatorTodos {
                    items: mine,
                    moved: Vec::new(),
                }))
            }
            TodoStop::Repo(i) => {
                self.repo_sel = i;
                self.repo_open = !self.repo_open;
                self.scroll_into_view(self.todos_row_of());
                ControlFlow::Break(None)
            }
            // Said, not silently nothing: the model moves its own row's status and the operator
            // may not, and a Tab that did nothing would read as a pane that stopped answering.
            TodoStop::Theirs(_) => {
                self.say("that row is the model's — nothing here moves it; enter reads it whole");
                ControlFlow::Break(None)
            }
        }
    }

    /// The person's queue verbs: approve, veto, remove, restart - each the SAME verb its typed
    /// spelling sends (`Action::Slash`), so key and verb cannot drift, and a row the daemon would
    /// refuse is said, not sent (`verb_moves`). Only on an empty composer, and on the row's own id.
    fn queue_verb(&mut self, verb: Verb) -> ControlFlow<Option<Action>> {
        let Some(id) = self.queue_selected_id() else {
            return ControlFlow::Continue(());
        };
        ControlFlow::Break(self.queue_verb_on(verb, &id))
    }

    /// One verb, on one entry: the key's act, and the act a plugin row carries. The row is looked up
    /// in the snapshot by id, because whether the daemon would take the verb is a question about
    /// the row's STATE (`verb_moves`); a row not in the last snapshot is said, not sent.
    pub(crate) fn queue_verb_on(&mut self, verb: Verb, id: &str) -> Option<Action> {
        let review = self.queues.reviews.iter().find(|r| r.id == id);
        let merge = self.queues.merges.iter().find(|m| m.id == id);
        let (selected, branch, repository, target) = match (review, merge) {
            (Some(r), _) => (
                Selected::Review(r),
                r.branch.clone(),
                r.repository.clone(),
                r.target.clone(),
            ),
            (None, Some(m)) => (
                Selected::Merge(m),
                m.branch.clone(),
                m.repository.clone(),
                m.target.clone(),
            ),
            (None, None) => {
                let said = format!(
                    "`{id}` is not in the last snapshot of the queue — it may have moved out of \
                     the window. Nothing was sent."
                );
                self.say(&said);
                return None;
            }
        };
        if let Err(why) = verb_moves(verb, &selected) {
            let said = format!(
                "`{branch}`: {} does nothing here — {why}. Nothing was sent.",
                verb.word()
            );
            self.say(&said);
            return None;
        }
        // `regate` is about the row's TARGET, named with its repository so the re-gate reaches the
        // project the row is in and not the session's own.
        let line = if verb == Verb::Regate {
            format!("queue regate {repository} {target}")
                .trim()
                .to_string()
        } else {
            // In the scope the pane shows: the daemon acts in this session's repository unless the
            // line names another, so a row the pane drew under `/queue all` is sent under `all`.
            format!("queue {}{} {id}", self.queue_scope.prefix(), verb.word())
        };
        Some(Action::Slash { line })
    }

    /// Enter on the jobs pane reads the row it is on, or folds the group - the shape the subagents
    /// pane keeps. The output opens in a pane, not in the conversation: the read is a
    /// `ReadJobOutput`, answered by a `JobOutput` event the overlay draws, with the list behind it.
    fn jobs_enter(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.job_stops();
        let Some(stop) = stops.get(self.jobs_sel.min(stops.len().saturating_sub(1))) else {
            return ControlFlow::Continue(());
        };
        match *stop {
            JobStop::Finished => {
                self.jobs_finished_open = !self.jobs_finished_open;
                ControlFlow::Break(None)
            }
            JobStop::Job(i) => {
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return ControlFlow::Break(None);
                }
                ControlFlow::Break(self.open_job_out(i))
            }
        }
    }

    /// The output window of job `i` of [`App::jobs`], opened on top of whatever is drawn: the jobs
    /// pane's Enter, and a click on a todo row's `waits on j97` link, so the two cannot come to
    /// show a job two ways.
    pub(crate) fn open_job_out(&mut self, i: usize) -> Option<Action> {
        let row = &self.jobs[i];
        let job = row.id.clone();
        // Where its output went, when it did not come here: the window for a redirected job is
        // empty by construction, so the pane needs the file's name.
        let redirect = row.redirect.clone();
        self.job_out = Some(JobOut {
            job: job.clone(),
            state: String::new(),
            never_ran: false,
            redirect,
            from: 0,
            to: 0,
            produced: 0,
            dropped: 0,
            lines: Vec::new(),
            next: None,
            back: Vec::new(),
            scroll: 0,
            loading: true,
            polling: false,
            asked_ms: self.now_ms,
            error: None,
        });
        self.push_surface(Surface::JobOut);
        Some(Action::ReadJobOutput { job, offset: 0 })
    }
}

/// The group line a row is drawn under, as an index into the drawn list - the nearest row ABOVE it
/// whose Enter folds something. `None` for a list with no group lines at all, which is the family
/// view and a daemon that holds one kind of session.
fn group_at(rows: &[crate::app::sessions_pane::Listed], at: usize) -> Option<usize> {
    rows[..at]
        .iter()
        .rposition(|r| matches!(r.enter, Some(letibot_plugin::pane::Act::Fold { .. })))
}
