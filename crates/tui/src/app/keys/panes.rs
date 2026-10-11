//! **What a surface's keys do**: the dispatch from a key to the focused surface's binding
//! (`keys::bindings`), and each surface's acts — the outputs (a subagent's, a job's, a slash
//! command's), the config pane, the overlays, the session picker, and the subagents, todos, queue,
//! standing and jobs panes.

use super::bindings::Act;
use super::*;
use crate::ui::panes::queue::{QRow, Selected, Verb, verb_moves};
use std::ops::ControlFlow;

/// The next stop from `at` in a list of `n`, wrapping — the one rule every list's arrows keep.
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
    /// **The keys of the surface in front, and of no other** (R69).
    ///
    /// The surface is [`App::focused_surface`] — the stack's top, unless a `!term` pane, the diff
    /// popup or rano is drawn over it — which is the same value `main_area` draws. A pane that is
    /// open behind another is not asked at all. That is the whole fix for the review's worst
    /// head finding: `ctrl-t` then `ctrl-g` drew the todos pane while ↑↓ and Enter went to the
    /// hidden subagents list, and **Enter switched session into a row nobody saw**, because the
    /// key order (subagents > todos > queue > jobs > standing) was a second list from the draw
    /// order (todos > queue > standing > subagents > jobs).
    ///
    /// **The key is looked up in the surface's table** (`Surface::bindings`): the first binding
    /// that names the key and whose guard holds is the one that runs. A key the table does not
    /// name — or a binding whose act has nothing to act on, an arrow in an empty list — falls
    /// through to what every surface shares:
    ///
    /// # Esc and ctrl-c pop
    ///
    /// An overlay's Esc goes back to its list because the list is the next entry down — not
    /// because its handler was placed above the block that closed every pane at once, which is
    /// what used to hold it there.
    ///
    /// # A surface owns Enter, even when it has nothing to act on
    ///
    /// The operator, 2026-10-03, having gone to the jobs pane and pressed Enter with a stray
    /// character in the composer: *"the pane own keyboard in a way, so enter is a pane thing."*
    /// A surface that names no meaning for Enter (help, `/status`, an empty jobs list) takes it
    /// anyway. **What is deliberately NOT here** is in [`Surface::owns_enter`]: where a typed
    /// line IS the answer — a row number, an id prefix — `submit` routes the words and the
    /// surface keeps the typed path it advertises. The merge queue was missing from the list
    /// this replaced, so Enter with typed text sent the prompt over an empty queue and opened an
    /// entry over a non-empty one.
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
                self.pop_surface();
                ControlFlow::Break(None)
            }
            Key::Enter if s.owns_enter() => ControlFlow::Break(None),
            _ => ControlFlow::Continue(()),
        }
    }

    /// **One act, on one surface.** `Continue` is the act declining — nothing to act on — and the
    /// key then falls through as though no binding had named it.
    pub(crate) fn surface_act(
        &mut self,
        s: Surface,
        act: Act,
        k: &Key,
    ) -> ControlFlow<Option<Action>> {
        let up = upward(k);
        match act {
            Act::Swallow => ControlFlow::Break(None),
            // **The window moves, in the surface's own units**: a line for an arrow, a page for a
            // page key, three lines a notch. An output view's tail through the one function that
            // knows its sign (`scroll_tail_overlay`); every pane drawn through `pane_window`
            // through `pane_scroll`, clamped against the last draw's own numbers — the key
            // handler has no width and no height, and a scroll clamped against a guess walks
            // past the end.
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
            Act::Open => self.open_act(s),
            Act::Alt => self.todos_alt(),
            Act::Peek => self.subagents_peek(),
            // **Enter reads the same subagent again**, because a running one has new output.
            Act::Reread => match self.sub_out.as_ref() {
                Some(v) => {
                    let id = v.session_id.clone();
                    self.sub_out_pending = Some(id.clone());
                    ControlFlow::Break(Some(Action::Peek(id)))
                }
                None => ControlFlow::Continue(()),
            },
            // **A job's output pages**: → and Enter ask for the page after the one on screen (or
            // re-read the end of a running job), ← walks back the way → came. `→` keeps the
            // empty-composer guard that Enter does not have: Enter here is the pane's, while `→`
            // is a cursor key first, and a half-typed line keeps its motion.
            Act::PageOut => ControlFlow::Break(self.job_out_page(!matches!(k, Key::Left))),
            Act::Expand | Act::Collapse => self.picker_fold(act == Act::Expand),
            // **`tab` moves between the queue's two views**, and the strip the pane draws is what
            // says so. It works over an empty queue too — the strip still says which half you
            // are looking at.
            Act::View => {
                self.queue_view = self.queue_view.other();
                self.redraw = true;
                ControlFlow::Break(None)
            }
            Act::Verb(v) => self.queue_verb(v),
        }
    }

    /// **↑↓ on a list**: the cursor moves through the list's ONE enumeration of stops — the same
    /// list the drawn `▸`, Enter and a click read — wrapping at the ends, and the window follows
    /// it, so an arrow that walked the cursor out of the window is not a key that seems to do
    /// nothing. An empty list declines.
    fn cursor_act(&mut self, s: Surface, up: bool) -> ControlFlow<Option<Action>> {
        match s {
            Surface::Picker => {
                let n = self.session_rows().len();
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
                let n = self.queue_rows().len();
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
            _ => return ControlFlow::Continue(()),
        }
        self.redraw = true;
        ControlFlow::Break(None)
    }

    /// **Enter on a list: the row's act.**
    fn open_act(&mut self, s: Surface) -> ControlFlow<Option<Action>> {
        match s {
            // The session under the cursor. The empty-composer rule is the binding's: a
            // half-typed id's Enter still means the id, and `submit` takes it.
            Surface::Picker => {
                let rows = self.session_rows();
                let Some(row) = rows.get(self.picker_sel.min(rows.len().saturating_sub(1))) else {
                    return ControlFlow::Continue(());
                };
                let id = self.sessions[row.idx].session_id.clone();
                ControlFlow::Break(self.switch_to(id))
            }
            // Changes the row under the cursor when it is one that can change now, and says why
            // when it is not.
            Surface::Config => ControlFlow::Break(self.config_change()),
            Surface::Todos => self.todos_read(),
            Surface::Queue => {
                // **What Enter opens is the ENTRY, and everything the queue knows about it**: the
                // ask it was produced under, the state and its reason, and the verdict's evidence
                // — the two rows the pane already holds, in full.
                let Some(row) = self.queue_selected() else {
                    return ControlFlow::Continue(());
                };
                self.queue_open = Some(self.queue_row_id(row));
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
            Surface::Subagents => self.subagents_enter(),
            Surface::Jobs => self.jobs_enter(),
            _ => ControlFlow::Continue(()),
        }
    }

    /// **→ and ← on the session picker: a conversation with sub-sessions opens and closes**, the
    /// two gestures a tree uses everywhere — and the reason a collapsed default can afford to be
    /// collapsed. **A child's ← closes its PARENT**, so `←` means *back up the tree* rather than
    /// nothing at all on the row you just arrived at.
    fn picker_fold(&mut self, open: bool) -> ControlFlow<Option<Action>> {
        let rows = self.session_rows();
        let Some(&row) = rows.get(self.picker_sel.min(rows.len().saturating_sub(1))) else {
            return ControlFlow::Continue(());
        };
        let want = if open {
            Some(self.sessions[row.idx].session_id.clone())
        } else {
            match row.depth {
                0 => Some(self.sessions[row.idx].session_id.clone()),
                _ => self.sessions[row.idx].parent_session_id.clone(),
            }
        };
        if let Some(want) = want {
            let at = self.expanded.iter().position(|e| *e == want);
            match (open, at) {
                (true, None) => self.expanded.push(want),
                (false, Some(pos)) => {
                    self.expanded.remove(pos);
                }
                _ => {}
            }
            self.redraw = true;
        }
        ControlFlow::Break(None)
    }

    /// **Enter on the subagent tree IS THE SWITCH INTO THAT SUBAGENT'S SESSION.** One keystroke,
    /// because that is what entering a row means everywhere else in this head and going into a
    /// subagent *is* going to that session — the operator, having driven into one and then been
    /// unable to get out again: *"when I \"Enter\" Subagent it is like completely switching
    /// session"*, and *"make \"o\" to \"Enter\""*. `o` stays bound to the same act, under the
    /// empty-composer guard every letter carries.
    ///
    /// **And the group row is the other thing Enter means here.** The finished children live
    /// under a fold (see [`App::subagent_stops`]); Enter on that row unfolds them, the same
    /// *Enter acts on what the cursor is on* rule the todos pane keeps.
    fn subagents_enter(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.subagent_stops();
        let Some(stop) = stops.get(self.subagents_sel.min(stops.len().saturating_sub(1))) else {
            return ControlFlow::Continue(());
        };
        match *stop {
            SubStop::Finished => {
                self.subagents_finished_open = !self.subagents_finished_open;
                self.redraw = true;
                ControlFlow::Break(None)
            }
            SubStop::Agent(i) => {
                let row = &self.subagents[i];
                if row.state == "opening" {
                    // Nothing to attach to yet, and the daemon would refuse the switch by name
                    // anyway; saying it here keeps the operator in the pane they were using.
                    self.say("that subagent is still opening — nothing to attach to yet");
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                let id = row.session_id.clone();
                self.close_surface(Surface::Subagents);
                ControlFlow::Break(self.switch_to(id))
            }
        }
    }

    /// **`p`: reading, not moving.** The output opens on the `Peeked` reply and this head never
    /// leaves the session it is in — R20's whole argument for the `Peek` frame. Nothing to read
    /// under the fold header, so `p` on it falls through.
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
            self.redraw = true;
            return ControlFlow::Break(None);
        }
        let id = row.session_id.clone();
        self.sub_out_pending = Some(id.clone());
        ControlFlow::Break(Some(Action::Peek(id)))
    }

    /// **Enter on the todos pane READS the row under the cursor** — the whole of it, in an
    /// overlay, which is the one place a row longer than the pane can be read at all. The
    /// operator: *"in todo list i cant expand your todo items - only truncated titles
    /// available"*. The add control is the exception, and it is the row's own nature: there is
    /// no text there to open, and its act is opening the card.
    ///
    /// **Under the same empty-composer guard the acts carry**: with a line typed, Enter is the
    /// composer's in this pane — a typed line IS the answer to the todos stops (`submit`).
    fn todos_read(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.todos_stops();
        let Some(stop) = stops.get(self.todos_sel.min(stops.len().saturating_sub(1))) else {
            return ControlFlow::Continue(());
        };
        match stop {
            TodoStop::Add => self.open_todo_card(),
            row => {
                self.todos_open = Some(row.clone());
                self.push_surface(Surface::TodoRow);
            }
        }
        ControlFlow::Break(None)
    }

    /// **Tab is the todos pane's own acts**, which is what it was beside Enter before the read
    /// took Enter: the card on the control, the toggle on one of the operator's rows, the unfold
    /// on a repo item. On the model's rows there is no act to perform — the R44 boundary, said
    /// rather than left as a key that does nothing. Tab is also the completion key while a
    /// `/command` is half-typed, which is what the binding's empty-composer guard keeps.
    fn todos_alt(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.todos_stops();
        let Some(stop) = stops
            .get(self.todos_sel.min(stops.len().saturating_sub(1)))
            .cloned()
        else {
            return ControlFlow::Continue(());
        };
        match stop {
            TodoStop::Add => {
                self.open_todo_card();
                ControlFlow::Break(None)
            }
            // **Marking done is the model's act too and it is the operator's own row**: the
            // daemon's `set_operator_states` is its own door and `/todo done N` is the typed one.
            // A completed row can be reopened, because a cursor that can only go one way is a
            // cursor you cannot correct.
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
                ControlFlow::Break(Some(Action::SetOperatorTodos {
                    items: mine,
                    moved: Vec::new(),
                }))
            }
            TodoStop::Repo(i) => {
                self.repo_sel = i;
                self.repo_open = !self.repo_open;
                self.scroll_into_view(self.todos_row_of());
                self.redraw = true;
                ControlFlow::Break(None)
            }
            // **Said, not silently nothing**: the model moves its own row's status and the
            // operator may not, and a Tab that did nothing would read as a pane that had stopped
            // answering.
            TodoStop::Theirs(_) => {
                self.say("that row is the model's — nothing here moves it; enter reads it whole");
                self.redraw = true;
                ControlFlow::Break(None)
            }
        }
    }

    /// **The person's queue verbs** — the operator's asks, in their words: *"i want to be able to
    /// approve / veto / delete"* and *"so merge queue has 4 failed items, we need a way to
    /// restart them"*. Each sends the SAME verb its typed spelling sends (`/queue approve|veto|rm|
    /// restart ID`, as `Action::Slash`), so the key and the verb cannot drift; and **a row the
    /// daemon would refuse is said, not sent** — through the same predicate the review view's
    /// verb row draws (`verb_moves`).
    ///
    /// **Only on an empty composer**, the binding's guard: typing "deploy" with `/queue` open
    /// sent `queue rm` on its `d` and `queue approve` on its `a` (R67 item 1).
    fn queue_verb(&mut self, verb: Verb) -> ControlFlow<Option<Action>> {
        let Some(row) = self.queue_selected() else {
            return ControlFlow::Continue(());
        };
        let (selected, branch) = match row {
            QRow::Review(i) => (
                Selected::Review(&self.queues.reviews[i]),
                self.queues.reviews[i].branch.clone(),
            ),
            QRow::Merge(i) => (
                Selected::Merge(&self.queues.merges[i]),
                self.queues.merges[i].branch.clone(),
            ),
        };
        if let Err(why) = verb_moves(verb, &selected) {
            let said = format!(
                "`{branch}`: {} does nothing here — {why}. Nothing was sent.",
                verb.word()
            );
            self.say(&said);
            self.redraw = true;
            return ControlFlow::Break(None);
        }
        // **`regate` is about the row's TARGET** (R76 item 3), named with its repository so the
        // re-gate reaches the project the row is in and not the session's own.
        let line = if verb == Verb::Regate {
            let (repo, target) = match row {
                QRow::Review(i) => (
                    self.queues.reviews[i].repository.clone(),
                    self.queues.reviews[i].target.clone(),
                ),
                QRow::Merge(i) => (
                    self.queues.merges[i].repository.clone(),
                    self.queues.merges[i].target.clone(),
                ),
            };
            format!("queue regate {repo} {target}").trim().to_string()
        } else {
            format!("queue {} {}", verb.word(), self.queue_row_id(row))
        };
        ControlFlow::Break(Some(Action::Slash { line }))
    }

    /// **Enter on the jobs pane reads the row it is on — or folds the group.** The same shape
    /// the subagents pane keeps, for the reason the operator gave: *"jobs panel - same as
    /// subagents - show list of running, group finished"*.
    ///
    /// **The output opens in a pane, not in the conversation.** This used to send `/job ID`,
    /// whose reply is a `Warning` on the session log — the operator, 2026-09-20: *"when i press
    /// enter on jobs pane im not shown the job output im brought back to the main conversation
    /// with /job <id> posted - this is not what i want"*. The read is a `ReadJobOutput`; it comes
    /// back as a `JobOutput` event and the overlay draws it, with the list behind it.
    fn jobs_enter(&mut self) -> ControlFlow<Option<Action>> {
        let stops = self.job_stops();
        let Some(stop) = stops.get(self.jobs_sel.min(stops.len().saturating_sub(1))) else {
            return ControlFlow::Continue(());
        };
        match *stop {
            JobStop::Finished => {
                self.jobs_finished_open = !self.jobs_finished_open;
                self.redraw = true;
                ControlFlow::Break(None)
            }
            JobStop::Job(i) => {
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                let row = &self.jobs[i];
                let job = row.id.clone();
                // **Where its output went, when it did not come here** (R41): the window for a
                // redirected job is empty by construction, so the pane needs the file's name.
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
                    error: None,
                });
                self.push_surface(Surface::JobOut);
                ControlFlow::Break(Some(Action::ReadJobOutput { job, offset: 0 }))
            }
        }
    }
}
